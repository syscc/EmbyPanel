use std::net::IpAddr;

use axum::{
    body::Body,
    http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri, header},
    response::{IntoResponse, Response},
};
use bytes::Bytes;

use crate::{
    auth,
    config::Config,
    error::{AppError, AppResult},
    rewrite,
};

pub async fn proxy_to_emby(
    client: &reqwest::Client,
    config: &Config,
    method: Method,
    uri: &Uri,
    headers: &HeaderMap,
    body: Bytes,
) -> AppResult<Response> {
    let target = config.emby_url(
        uri.path_and_query()
            .map(|value| value.as_str())
            .unwrap_or("/"),
    )?;
    let mut request = client.request(reqwest_method(&method)?, target);

    request = forward_request_headers(request, config, headers);

    if method != Method::GET && method != Method::HEAD {
        request = request.body(body);
    }

    let response = request.send().await?;
    response_to_axum(response).await
}

pub async fn proxy_text(
    client: &reqwest::Client,
    config: &Config,
    method: Method,
    uri: &Uri,
    headers: &HeaderMap,
    body: Bytes,
) -> AppResult<(StatusCode, HeaderMap, String)> {
    let response = proxy_to_reqwest(client, config, method, uri, headers, body).await?;
    let status = rewrite::status_from_reqwest(response.status());
    let headers = copy_response_headers(response.headers());
    let text = response.text().await?;
    Ok((status, headers, text))
}

async fn proxy_to_reqwest(
    client: &reqwest::Client,
    config: &Config,
    method: Method,
    uri: &Uri,
    headers: &HeaderMap,
    body: Bytes,
) -> AppResult<reqwest::Response> {
    let target = config.emby_url(
        uri.path_and_query()
            .map(|value| value.as_str())
            .unwrap_or("/"),
    )?;
    let mut request = client.request(reqwest_method(&method)?, target);

    request = forward_request_headers(request, config, headers);

    if method != Method::GET && method != Method::HEAD {
        request = request.body(body);
    }

    Ok(request.send().await?)
}

async fn response_to_axum(response: reqwest::Response) -> AppResult<Response> {
    let status = rewrite::status_from_reqwest(response.status());
    let headers = copy_response_headers(response.headers());
    let bytes = response.bytes().await?;
    let mut builder = Response::builder().status(status);

    for (name, value) in headers {
        if let Some(name) = name {
            builder = builder.header(name, value);
        }
    }

    builder
        .body(Body::from(bytes))
        .map_err(|err| AppError::Internal(format!("failed to build proxy response: {err}")))
}

pub fn redirect_response(location: String) -> Response {
    (StatusCode::FOUND, [(header::LOCATION, location)]).into_response()
}

pub fn body_response(
    status: StatusCode,
    headers: HeaderMap,
    body: impl Into<Body>,
) -> AppResult<Response> {
    let mut builder = Response::builder().status(status);
    for (name, value) in headers {
        if let Some(name) = name {
            builder = builder.header(name, value);
        }
    }
    builder
        .body(body.into())
        .map_err(|err| AppError::Internal(format!("failed to build response: {err}")))
}

fn copy_response_headers(headers: &reqwest::header::HeaderMap) -> HeaderMap {
    let mut out = HeaderMap::new();
    for (name, value) in headers {
        if should_copy_response_header(name.as_str())
            && let Ok(header_name) = axum::http::HeaderName::from_bytes(name.as_str().as_bytes())
            && let Ok(header_value) = HeaderValue::from_bytes(value.as_bytes())
        {
            out.append(header_name, header_value);
        }
    }
    out
}

fn reqwest_method(method: &Method) -> AppResult<reqwest::Method> {
    reqwest::Method::from_bytes(method.as_str().as_bytes())
        .map_err(|err| AppError::Internal(format!("invalid request method: {err}")))
}

fn forward_request_headers(
    mut request: reqwest::RequestBuilder,
    config: &Config,
    headers: &HeaderMap,
) -> reqwest::RequestBuilder {
    let real_ip = configured_real_ip(config, headers);
    for (name, value) in headers {
        let header_name = name.as_str();
        if should_forward_header_with_real_ip_rewrite(header_name, real_ip.is_some()) {
            if header_name.eq_ignore_ascii_case("cookie") {
                if let Some(cookie) = filtered_proxy_cookie(value) {
                    request = request.header(header_name, cookie);
                }
            } else {
                request = request.header(header_name, value.as_bytes());
            }
        }
    }
    if let Some(real_ip) = real_ip {
        request = request
            .header("X-Real-IP", real_ip.as_str())
            .header("X-Forwarded-For", real_ip.as_str());
    }
    request
}

fn filtered_proxy_cookie(value: &HeaderValue) -> Option<String> {
    let value = value.to_str().ok()?;
    let filtered = value
        .split(';')
        .filter_map(|part| {
            let part = part.trim();
            let (name, _) = part.split_once('=')?;
            (!name.trim().eq_ignore_ascii_case(auth::TOKEN_COOKIE_NAME)).then_some(part)
        })
        .collect::<Vec<_>>()
        .join("; ");
    (!filtered.is_empty()).then_some(filtered)
}

fn should_forward_header_with_real_ip_rewrite(name: &str, rewrite_real_ip_headers: bool) -> bool {
    if rewrite_real_ip_headers
        && matches!(
            name.to_ascii_lowercase().as_str(),
            "x-real-ip" | "x-forwarded-for" | "forwarded"
        )
    {
        return false;
    }
    !matches!(
        name.to_ascii_lowercase().as_str(),
        "host" | "content-length" | "connection" | "accept-encoding"
    )
}

fn configured_real_ip(config: &Config, headers: &HeaderMap) -> Option<String> {
    let server = config.servers.first()?;
    match server.real_ip_mode.as_str() {
        "header" => header_ip(headers, &server.real_ip_header),
        "header_list" => header_list_ip(headers, &server.real_ip_header),
        "xff_last" => x_forwarded_for_ip(headers, 1),
        "xff_second_last" => x_forwarded_for_ip(headers, 2),
        "xff_third_last" => x_forwarded_for_ip(headers, 3),
        _ => None,
    }
}

pub fn real_ip_for_log(config: &Config, headers: &HeaderMap) -> Option<String> {
    configured_real_ip(config, headers)
}

fn header_list_ip(headers: &HeaderMap, configured_headers: &str) -> Option<String> {
    let configured = configured_headers
        .lines()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .collect::<Vec<_>>();
    let fallback = [
        "x-forwarded-for",
        "x-real-ip",
        "x-forwarded",
        "forwarded-for",
        "forwarded",
        "true-client-ip",
        "client-ip",
        "ali-cdn-real-ip",
        "cdn-src-ip",
        "cdn-real-ip",
        "cf-connecting-ip",
        "x-cluster-client-ip",
        "wl-proxy-client-ip",
        "proxy-client-ip",
    ];
    let names = if configured.is_empty() {
        fallback.as_slice()
    } else {
        configured.as_slice()
    };
    names.iter().find_map(|name| header_ip(headers, name))
}

fn header_ip(headers: &HeaderMap, header_name: &str) -> Option<String> {
    let header_name = HeaderName::from_bytes(header_name.trim().as_bytes()).ok()?;
    headers
        .get(header_name)
        .and_then(|value| value.to_str().ok())
        .and_then(first_header_ip)
}

fn x_forwarded_for_ip(headers: &HeaderMap, reverse_index: usize) -> Option<String> {
    let value = headers
        .get("x-forwarded-for")
        .and_then(|value| value.to_str().ok())?;
    let ips = header_ip_list(value);
    ips.len()
        .checked_sub(reverse_index)
        .and_then(|index| ips.get(index))
        .cloned()
}

fn first_header_ip(value: &str) -> Option<String> {
    header_ip_list(value).into_iter().next()
}

fn header_ip_list(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(|item| item.trim().trim_matches('"'))
        .filter(|item| !item.is_empty())
        .filter_map(|item| item.split(';').next())
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .filter_map(normalize_header_ip_value)
        .collect()
}

fn normalize_header_ip_value(value: &str) -> Option<String> {
    let mut value = value
        .trim()
        .trim_start_matches("for=")
        .trim_matches('"')
        .trim();
    if value.starts_with('[') {
        let end = value.find(']')?;
        value = &value[1..end];
    } else if let Some((host, port)) = value.rsplit_once(':')
        && !host.contains(':')
        && port.chars().all(|ch| ch.is_ascii_digit())
    {
        value = host;
    }
    value.parse::<IpAddr>().ok().map(|ip| ip.to_string())
}

fn should_copy_response_header(name: &str) -> bool {
    !matches!(
        name.to_ascii_lowercase().as_str(),
        "content-length" | "transfer-encoding" | "connection" | "content-encoding"
    )
}

#[cfg(test)]
mod tests {
    use reqwest::header::{HeaderMap as ReqwestHeaderMap, HeaderValue as ReqwestHeaderValue};

    use crate::config::EmbyServerConfig;

    use super::*;

    fn test_config(real_ip_mode: &str, real_ip_header: &str) -> Config {
        Config {
            emby_host: "http://emby.test".to_string(),
            emby_api_key: "key".to_string(),
            servers: vec![EmbyServerConfig {
                id: "server-1".to_string(),
                name: "server".to_string(),
                emby_host: "http://emby.test".to_string(),
                emby_api_key: "key".to_string(),
                port: 18096,
                enabled: true,
                real_ip_mode: real_ip_mode.to_string(),
                real_ip_header: real_ip_header.to_string(),
            }],
            openlist_addr: None,
            openlist_token: None,
            port: 18096,
            cache_ttl_seconds: 180,
            cache_max_capacity: 10_000,
            cache_domain_filter_mode: "off".to_string(),
            cache_domain_whitelist: String::new(),
            cache_domain_blacklist: String::new(),
            enable_internal_redirect: false,
            internal_redirect_timeout_seconds: 15,
            strm_url_mappings: String::new(),
            connectivity_check_enabled: true,
            connectivity_check_interval_seconds: 60,
            connectivity_check_timeout_seconds: 5,
            connectivity_auto_restart_seconds: 180,
            strm_url_mapping_rules: Vec::new(),
        }
    }

    #[test]
    fn response_headers_keep_multiple_set_cookie_values() {
        let mut headers = ReqwestHeaderMap::new();
        headers.append(
            reqwest::header::SET_COOKIE,
            ReqwestHeaderValue::from_static("a=1; Path=/"),
        );
        headers.append(
            reqwest::header::SET_COOKIE,
            ReqwestHeaderValue::from_static("b=2; Path=/"),
        );

        let copied = copy_response_headers(&headers);
        let cookies = copied
            .get_all(header::SET_COOKIE)
            .iter()
            .map(|value| value.to_str().unwrap())
            .collect::<Vec<_>>();

        assert_eq!(cookies, vec!["a=1; Path=/", "b=2; Path=/"]);
    }

    #[test]
    fn compression_headers_are_not_forwarded_after_reqwest_decodes_body() {
        assert!(!should_forward_header_with_real_ip_rewrite(
            "accept-encoding",
            false
        ));
        assert!(!should_copy_response_header("content-encoding"));
    }

    #[test]
    fn proxy_cookie_filter_removes_panel_session_token() {
        let value = HeaderValue::from_static("a=1; embypanel_token=secret; b=2");
        assert_eq!(filtered_proxy_cookie(&value).as_deref(), Some("a=1; b=2"));

        let value = HeaderValue::from_static("embypanel_token=secret");
        assert_eq!(filtered_proxy_cookie(&value), None);
    }

    #[test]
    fn configured_real_ip_reads_named_header() {
        let mut headers = HeaderMap::new();
        headers.insert("cf-connecting-ip", HeaderValue::from_static("203.0.113.10"));

        assert_eq!(
            configured_real_ip(&test_config("header", "CF-Connecting-IP"), &headers).as_deref(),
            Some("203.0.113.10")
        );
    }

    #[test]
    fn configured_real_ip_reads_x_forwarded_for_reverse_position() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("198.51.100.1, 198.51.100.2, 198.51.100.3"),
        );

        assert_eq!(
            configured_real_ip(&test_config("xff_last", ""), &headers).as_deref(),
            Some("198.51.100.3")
        );
        assert_eq!(
            configured_real_ip(&test_config("xff_second_last", ""), &headers).as_deref(),
            Some("198.51.100.2")
        );
        assert_eq!(
            configured_real_ip(&test_config("xff_third_last", ""), &headers).as_deref(),
            Some("198.51.100.1")
        );
    }

    #[test]
    fn auto_real_ip_mode_does_not_trust_client_forwarded_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", HeaderValue::from_static("203.0.113.10"));

        assert_eq!(real_ip_for_log(&test_config("auto", ""), &headers), None);
    }

    #[test]
    fn configured_real_ip_rejects_invalid_header_values() {
        let mut headers = HeaderMap::new();
        headers.insert("cf-connecting-ip", HeaderValue::from_static("not-an-ip"));

        assert_eq!(
            configured_real_ip(&test_config("header", "CF-Connecting-IP"), &headers),
            None
        );
    }

    #[test]
    fn configured_real_ip_accepts_forwarded_ip_formats() {
        assert_eq!(
            header_ip_list("for=\"[2001:db8::1]:443\", for=198.51.100.7:1234"),
            vec!["2001:db8::1".to_string(), "198.51.100.7".to_string()]
        );
    }
}
