use std::{collections::HashSet, net::IpAddr, time::Duration};

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

const UPSTREAM_RESPONSE_HEADERS_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_X_FORWARDED_FOR_ENTRIES: usize = 16;
const FORWARDED_IP_HEADERS: &[&str] = &[
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

pub async fn proxy_to_emby(
    client: &reqwest::Client,
    config: &Config,
    method: Method,
    uri: &Uri,
    headers: &HeaderMap,
    body: Bytes,
    peer_ip: IpAddr,
) -> AppResult<Response> {
    let target = config.emby_url(
        uri.path_and_query()
            .map(|value| value.as_str())
            .unwrap_or("/"),
    )?;
    let mut request = client.request(reqwest_method(&method)?, target);

    request = forward_request_headers(request, config, headers, peer_ip);

    if method != Method::GET && method != Method::HEAD {
        request = request.body(body);
    }

    let response = send_upstream(request).await?;
    response_to_axum(response)
}

pub async fn proxy_text(
    client: &reqwest::Client,
    config: &Config,
    method: Method,
    uri: &Uri,
    headers: &HeaderMap,
    body: Bytes,
    peer_ip: IpAddr,
) -> AppResult<(StatusCode, HeaderMap, String)> {
    let response = proxy_to_reqwest(client, config, method, uri, headers, body, peer_ip).await?;
    let status = rewrite::status_from_reqwest(response.status());
    let headers = copy_response_headers(response.headers(), false);
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
    peer_ip: IpAddr,
) -> AppResult<reqwest::Response> {
    let target = config.emby_url(
        uri.path_and_query()
            .map(|value| value.as_str())
            .unwrap_or("/"),
    )?;
    let mut request = client.request(reqwest_method(&method)?, target);

    request = forward_request_headers(request, config, headers, peer_ip);

    if method != Method::GET && method != Method::HEAD {
        request = request.body(body);
    }

    send_upstream(request).await
}

async fn send_upstream(request: reqwest::RequestBuilder) -> AppResult<reqwest::Response> {
    tokio::time::timeout(UPSTREAM_RESPONSE_HEADERS_TIMEOUT, request.send())
        .await
        .map_err(|_| AppError::BadGateway("Emby upstream response headers timed out".to_string()))?
        .map_err(AppError::from)
}

fn response_to_axum(response: reqwest::Response) -> AppResult<Response> {
    let status = rewrite::status_from_reqwest(response.status());
    let headers = copy_response_headers(response.headers(), true);
    body_response(status, headers, Body::from_stream(response.bytes_stream()))
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

fn copy_response_headers(
    headers: &reqwest::header::HeaderMap,
    preserve_representation_headers: bool,
) -> HeaderMap {
    let connection_headers = connection_header_names(headers);
    let mut out = HeaderMap::new();
    for (name, value) in headers {
        if should_copy_response_header(
            name.as_str(),
            &connection_headers,
            preserve_representation_headers,
        ) && let Ok(header_name) = axum::http::HeaderName::from_bytes(name.as_str().as_bytes())
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
    peer_ip: IpAddr,
) -> reqwest::RequestBuilder {
    let real_ip = resolved_client_ip(config, headers, peer_ip);
    let forwarded_for = forwarded_for_value(config, headers, peer_ip, real_ip);
    let connection_headers = connection_header_names(headers);
    for (name, value) in headers {
        let header_name = name.as_str();
        if should_forward_request_header(header_name, &connection_headers, config) {
            if header_name.eq_ignore_ascii_case("cookie") {
                if let Some(cookie) = filtered_proxy_cookie(value) {
                    request = request.header(header_name, cookie);
                }
            } else {
                request = request.header(header_name, value.as_bytes());
            }
        }
    }
    request
        .header("X-Real-IP", real_ip.to_string())
        .header("X-Forwarded-For", forwarded_for)
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

fn should_forward_request_header(
    name: &str,
    connection_headers: &HashSet<String>,
    config: &Config,
) -> bool {
    let name = name.to_ascii_lowercase();
    if is_hop_by_hop_header(&name)
        || connection_headers.contains(&name)
        || is_forwarded_ip_header(config, &name)
        || matches!(
            name.as_str(),
            "x-forwarded-host" | "x-forwarded-proto" | "x-forwarded-port"
        )
    {
        return false;
    }
    !matches!(name.as_str(), "host" | "content-length" | "accept-encoding")
}

fn configured_real_ip(config: &Config, headers: &HeaderMap, peer_ip: IpAddr) -> Option<IpAddr> {
    let server = config.servers.first()?;
    if !server.is_trusted_proxy(peer_ip) {
        return None;
    }
    match server.real_ip_mode.as_str() {
        "header" => header_ip(headers, &server.real_ip_header),
        "header_list" => header_list_ip(headers, &server.real_ip_header),
        "xff_last" => x_forwarded_for_ip(headers, 1),
        "xff_second_last" => x_forwarded_for_ip(headers, 2),
        "xff_third_last" => x_forwarded_for_ip(headers, 3),
        _ => None,
    }
}

fn resolved_client_ip(config: &Config, headers: &HeaderMap, peer_ip: IpAddr) -> IpAddr {
    let peer_ip = canonical_ip(peer_ip);
    configured_real_ip(config, headers, peer_ip).unwrap_or(peer_ip)
}

fn forwarded_for_value(
    config: &Config,
    headers: &HeaderMap,
    peer_ip: IpAddr,
    real_ip: IpAddr,
) -> String {
    let peer_ip = canonical_ip(peer_ip);
    let trusted_peer = config
        .servers
        .first()
        .is_some_and(|server| server.is_trusted_proxy(peer_ip));
    if !trusted_peer {
        return real_ip.to_string();
    }

    let mut chain = headers
        .get_all("x-forwarded-for")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(header_ip_list)
        .collect::<Vec<_>>();
    if !chain.contains(&real_ip) {
        chain.insert(0, real_ip);
    }
    if chain.last().copied() != Some(peer_ip) {
        chain.push(peer_ip);
    }
    while chain.len() > MAX_X_FORWARDED_FOR_ENTRIES {
        chain.remove(1);
    }
    chain
        .into_iter()
        .map(|ip| ip.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn real_ip_for_log(config: &Config, headers: &HeaderMap, peer_ip: IpAddr) -> String {
    resolved_client_ip(config, headers, peer_ip).to_string()
}

fn canonical_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(ip) => ip
            .to_ipv4_mapped()
            .map(IpAddr::V4)
            .unwrap_or(IpAddr::V6(ip)),
        ip => ip,
    }
}

fn header_list_ip(headers: &HeaderMap, configured_headers: &str) -> Option<IpAddr> {
    let configured = configured_headers
        .lines()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .collect::<Vec<_>>();
    let names = if configured.is_empty() {
        FORWARDED_IP_HEADERS
    } else {
        configured.as_slice()
    };
    names.iter().find_map(|name| header_ip(headers, name))
}

fn header_ip(headers: &HeaderMap, header_name: &str) -> Option<IpAddr> {
    let header_name = HeaderName::from_bytes(header_name.trim().as_bytes()).ok()?;
    headers
        .get(header_name)
        .and_then(|value| value.to_str().ok())
        .and_then(first_header_ip)
}

fn x_forwarded_for_ip(headers: &HeaderMap, reverse_index: usize) -> Option<IpAddr> {
    let ips = headers
        .get_all("x-forwarded-for")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(header_ip_list)
        .collect::<Vec<_>>();
    ips.len()
        .checked_sub(reverse_index)
        .and_then(|index| ips.get(index))
        .cloned()
}

fn first_header_ip(value: &str) -> Option<IpAddr> {
    header_ip_list(value).into_iter().next()
}

fn header_ip_list(value: &str) -> Vec<IpAddr> {
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

fn normalize_header_ip_value(value: &str) -> Option<IpAddr> {
    let mut value = value.trim();
    if let Some((name, forwarded_value)) = value.split_once('=')
        && name.trim().eq_ignore_ascii_case("for")
    {
        value = forwarded_value;
    }
    value = value.trim_matches('"').trim();
    if value.starts_with('[') {
        let end = value.find(']')?;
        value = &value[1..end];
    } else if let Some((host, port)) = value.rsplit_once(':')
        && !host.contains(':')
        && port.chars().all(|ch| ch.is_ascii_digit())
    {
        value = host;
    }
    value.parse::<IpAddr>().ok().map(canonical_ip)
}

fn connection_header_names(headers: &HeaderMap) -> HashSet<String> {
    headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_ascii_lowercase)
        .collect()
}

fn is_hop_by_hop_header(name: &str) -> bool {
    matches!(
        name,
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "proxy-connection"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
    )
}

fn is_forwarded_ip_header(config: &Config, name: &str) -> bool {
    FORWARDED_IP_HEADERS
        .iter()
        .any(|header| name.eq_ignore_ascii_case(header))
        || config.servers.first().is_some_and(|server| {
            server
                .real_ip_header
                .lines()
                .any(|header| name.eq_ignore_ascii_case(header.trim()))
        })
}

fn should_copy_response_header(
    name: &str,
    connection_headers: &HashSet<String>,
    preserve_representation_headers: bool,
) -> bool {
    let name = name.to_ascii_lowercase();
    !is_hop_by_hop_header(&name)
        && !connection_headers.contains(&name)
        && (preserve_representation_headers
            || !matches!(name.as_str(), "content-length" | "content-encoding"))
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
                block_web_ui: false,
                real_ip_mode: real_ip_mode.to_string(),
                real_ip_header: real_ip_header.to_string(),
                trusted_proxy_cidrs: "192.0.2.0/24".to_string(),
                trusted_proxy_networks: vec!["192.0.2.0/24".parse().unwrap()],
            }],
            openlist_addr: None,
            openlist_token: None,
            port: 18096,
            cache_ttl_seconds: 180,
            cache_max_capacity: 10_000,
            cache_enabled: true,
            cache_domain_filter_mode: "off".to_string(),
            cache_domain_whitelist: String::new(),
            cache_domain_blacklist: String::new(),
            enable_internal_redirect: false,
            internal_redirect_timeout_seconds: 15,
            strm_url_mappings: String::new(),
            strm_url_mapping_enabled: true,
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

        let copied = copy_response_headers(&headers, true);
        let cookies = copied
            .get_all(header::SET_COOKIE)
            .iter()
            .map(|value| value.to_str().unwrap())
            .collect::<Vec<_>>();

        assert_eq!(cookies, vec!["a=1; Path=/", "b=2; Path=/"]);
    }

    #[test]
    fn compression_headers_are_not_forwarded_after_reqwest_decodes_body() {
        let config = test_config("auto", "");
        assert!(!should_forward_request_header(
            "accept-encoding",
            &HashSet::new(),
            &config,
        ));
        assert!(!should_copy_response_header(
            "content-encoding",
            &HashSet::new(),
            false,
        ));
        assert!(should_copy_response_header(
            "content-encoding",
            &HashSet::new(),
            true,
        ));
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
            configured_real_ip(
                &test_config("header", "CF-Connecting-IP"),
                &headers,
                "192.0.2.10".parse().unwrap(),
            ),
            Some("203.0.113.10".parse().unwrap())
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
            configured_real_ip(
                &test_config("xff_last", ""),
                &headers,
                "192.0.2.10".parse().unwrap(),
            ),
            Some("198.51.100.3".parse().unwrap())
        );
        assert_eq!(
            configured_real_ip(
                &test_config("xff_second_last", ""),
                &headers,
                "192.0.2.10".parse().unwrap(),
            ),
            Some("198.51.100.2".parse().unwrap())
        );
        assert_eq!(
            configured_real_ip(
                &test_config("xff_third_last", ""),
                &headers,
                "192.0.2.10".parse().unwrap(),
            ),
            Some("198.51.100.1".parse().unwrap())
        );
    }

    #[test]
    fn auto_real_ip_mode_does_not_trust_client_forwarded_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", HeaderValue::from_static("203.0.113.10"));

        assert_eq!(
            real_ip_for_log(
                &test_config("auto", ""),
                &headers,
                "192.0.2.10".parse().unwrap(),
            ),
            "192.0.2.10",
        );
    }

    #[test]
    fn configured_real_ip_rejects_invalid_header_values() {
        let mut headers = HeaderMap::new();
        headers.insert("cf-connecting-ip", HeaderValue::from_static("not-an-ip"));

        assert_eq!(
            configured_real_ip(
                &test_config("header", "CF-Connecting-IP"),
                &headers,
                "192.0.2.10".parse().unwrap(),
            ),
            None
        );
    }

    #[test]
    fn configured_real_ip_accepts_forwarded_ip_formats() {
        assert_eq!(
            header_ip_list("for=\"[2001:db8::1]:443\", for=198.51.100.7:1234"),
            vec![
                "2001:db8::1".parse::<IpAddr>().unwrap(),
                "198.51.100.7".parse::<IpAddr>().unwrap(),
            ]
        );
    }

    #[test]
    fn untrusted_peer_cannot_spoof_forwarded_client_ip() {
        let mut headers = HeaderMap::new();
        headers.insert("cf-connecting-ip", HeaderValue::from_static("203.0.113.10"));
        let config = test_config("header", "CF-Connecting-IP");
        let peer_ip = "198.51.100.20".parse().unwrap();

        assert_eq!(configured_real_ip(&config, &headers, peer_ip), None);
        assert_eq!(real_ip_for_log(&config, &headers, peer_ip), "198.51.100.20");
    }

    #[test]
    fn proxy_rewrites_forwarded_headers_and_drops_connection_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("cf-connecting-ip", HeaderValue::from_static("203.0.113.10"));
        headers.insert(header::USER_AGENT, HeaderValue::from_static("test-player"));
        headers.insert(
            "x-forwarded-host",
            HeaderValue::from_static("attacker.example.test"),
        );
        headers.insert("x-forwarded-proto", HeaderValue::from_static("https"));
        headers.insert(header::CONNECTION, HeaderValue::from_static("x-remove-me"));
        headers.insert("x-remove-me", HeaderValue::from_static("secret"));
        let config = test_config("header", "CF-Connecting-IP");
        let request = forward_request_headers(
            reqwest::Client::new().get("http://emby.test/items"),
            &config,
            &headers,
            "192.0.2.10".parse().unwrap(),
        )
        .build()
        .unwrap();

        assert_eq!(request.headers()["x-real-ip"], "203.0.113.10");
        assert_eq!(
            request.headers()["x-forwarded-for"],
            "203.0.113.10, 192.0.2.10"
        );
        assert_eq!(request.headers()[header::USER_AGENT], "test-player");
        assert!(!request.headers().contains_key("cf-connecting-ip"));
        assert!(!request.headers().contains_key("x-forwarded-host"));
        assert!(!request.headers().contains_key("x-forwarded-proto"));
        assert!(!request.headers().contains_key("x-remove-me"));
        assert!(!request.headers().contains_key(header::CONNECTION));
    }

    #[test]
    fn configured_real_ip_combines_repeated_x_forwarded_for_fields() {
        let mut headers = HeaderMap::new();
        headers.append(
            "x-forwarded-for",
            HeaderValue::from_static("198.51.100.1, 198.51.100.2"),
        );
        headers.append("x-forwarded-for", HeaderValue::from_static("198.51.100.3"));
        let config = test_config("xff_second_last", "");

        assert_eq!(
            configured_real_ip(&config, &headers, "192.0.2.10".parse().unwrap()),
            Some("198.51.100.2".parse().unwrap())
        );
    }

    #[test]
    fn trusted_proxy_preserves_a_bounded_normalized_forwarded_chain() {
        let mut headers = HeaderMap::new();
        headers.append(
            "x-forwarded-for",
            HeaderValue::from_static("198.51.100.1, 198.51.100.2"),
        );
        headers.append("x-forwarded-for", HeaderValue::from_static("invalid"));
        let config = test_config("xff_last", "");
        let request = forward_request_headers(
            reqwest::Client::new().get("http://emby.test/items"),
            &config,
            &headers,
            "192.0.2.10".parse().unwrap(),
        )
        .build()
        .unwrap();

        assert_eq!(
            request.headers()["x-forwarded-for"],
            "198.51.100.1, 198.51.100.2, 192.0.2.10"
        );
    }

    #[test]
    fn untrusted_peer_forwarding_chain_is_replaced() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("198.51.100.1, 198.51.100.2"),
        );
        let config = test_config("xff_last", "");
        let request = forward_request_headers(
            reqwest::Client::new().get("http://emby.test/items"),
            &config,
            &headers,
            "203.0.113.20".parse().unwrap(),
        )
        .build()
        .unwrap();

        assert_eq!(request.headers()["x-real-ip"], "203.0.113.20");
        assert_eq!(request.headers()["x-forwarded-for"], "203.0.113.20");
    }

    #[test]
    fn streamed_response_preserves_range_headers_and_drops_hop_by_hop_headers() {
        let mut headers = ReqwestHeaderMap::new();
        headers.insert(
            reqwest::header::CONTENT_LENGTH,
            ReqwestHeaderValue::from_static("42"),
        );
        headers.insert(
            reqwest::header::CONTENT_RANGE,
            ReqwestHeaderValue::from_static("bytes 0-41/100"),
        );
        headers.insert(
            reqwest::header::CONNECTION,
            ReqwestHeaderValue::from_static("x-remove-me"),
        );
        headers.insert("x-remove-me", ReqwestHeaderValue::from_static("secret"));

        let copied = copy_response_headers(&headers, true);

        assert_eq!(copied[header::CONTENT_LENGTH], "42");
        assert_eq!(copied[header::CONTENT_RANGE], "bytes 0-41/100");
        assert!(!copied.contains_key(header::CONNECTION));
        assert!(!copied.contains_key("x-remove-me"));
    }
}
