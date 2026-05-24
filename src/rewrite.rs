use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use regex::Regex;
use serde_json::{Value, json};
use url::form_urlencoded;

pub fn is_base_html_player(path: &str) -> bool {
    path.ends_with("/basehtmlplayer.js")
}

pub fn is_system_info(path: &str) -> bool {
    path.to_ascii_lowercase().ends_with("/system/info")
}

pub fn is_playback_info(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.ends_with("/playbackinfo") && lower.contains("/items/")
}

pub fn is_video_stream(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.contains("/videos/") && (lower.contains("/stream") || lower.contains("/original"))
}

pub fn parse_item_id(path: &str) -> Option<String> {
    let mut segments = path
        .trim_start_matches('/')
        .split('/')
        .filter(|segment| !segment.is_empty());

    while let Some(segment) = segments.next() {
        if segment.eq_ignore_ascii_case("videos") || segment.eq_ignore_ascii_case("items") {
            return segments.next().map(str::to_string);
        }
    }

    None
}

pub fn source_label(media_source_id: Option<&str>) -> &str {
    media_source_id.unwrap_or("default")
}

pub fn patch_base_html_player(body: &str) -> String {
    let pattern = Regex::new(
        r#"mediaSource\.IsRemote\s*&&\s*"DirectPlay"\s*===\s*playMethod\s*\?\s*null\s*:\s*"anonymous""#,
    )
    .expect("valid basehtmlplayer rewrite regex");
    pattern.replace_all(body, "null").into_owned()
}

pub fn patch_system_info(mut body: Value, gateway_port: u16, gateway_url: &str) -> Value {
    if let Some(object) = body.as_object_mut() {
        object.insert("WebSocketPortNumber".to_string(), json!(gateway_port));
        object.insert("HttpServerPortNumber".to_string(), json!(gateway_port));
        object.insert("LocalAddress".to_string(), json!(gateway_url));
        object.insert("LocalAddresses".to_string(), json!([gateway_url]));
        object.insert("RemoteAddress".to_string(), json!(gateway_url));
        object.insert("RemoteAddresses".to_string(), json!([gateway_url]));
        object.insert("WanAddress".to_string(), json!(gateway_url));
    }
    body
}

pub fn patch_playback_info(mut body: Value, path: &str, query: Option<&str>) -> Value {
    let Some(item_id) = parse_item_id(path) else {
        return body;
    };

    let Some(media_sources) = body
        .get_mut("MediaSources")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return body;
    };

    for source in media_sources {
        if !is_strm_media_source(source) {
            continue;
        }

        let Some(object) = source.as_object_mut() else {
            continue;
        };

        object.insert("SupportsDirectPlay".to_string(), json!(true));
        object.insert("SupportsDirectStream".to_string(), json!(true));
        object.insert("SupportsTranscoding".to_string(), json!(false));
        object.remove("TranscodingUrl");
        object.remove("TranscodingSubProtocol");
        object.remove("TranscodingContainer");

        let Some(source_id) = object.get("Id").and_then(Value::as_str) else {
            continue;
        };

        let direct_stream_url = build_direct_stream_url(&item_id, source_id, query);
        object.insert("DirectStreamUrl".to_string(), json!(direct_stream_url));
    }

    body
}

pub fn is_strm_media_source(source: &Value) -> bool {
    source
        .get("IsRemote")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        && !source
            .get("IsInfiniteStream")
            .and_then(Value::as_bool)
            .unwrap_or(false)
}

pub fn json_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    headers
}

pub fn text_headers(content_type: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(content_type)
            .unwrap_or_else(|_| HeaderValue::from_static("text/plain")),
    );
    headers
}

pub fn status_from_reqwest(status: reqwest::StatusCode) -> StatusCode {
    StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY)
}

fn build_direct_stream_url(item_id: &str, source_id: &str, query: Option<&str>) -> String {
    let mut params = form_urlencoded::Serializer::new(String::new());
    let mut has_api_key = false;
    let mut has_token = false;

    if let Some(query) = query {
        for (key, value) in form_urlencoded::parse(query.as_bytes()) {
            if key.eq_ignore_ascii_case("api_key") {
                has_api_key = true;
            }
            if key.eq_ignore_ascii_case("X-Emby-Token") {
                has_token = true;
            }
            if !key.eq_ignore_ascii_case("MediaSourceId") && !key.eq_ignore_ascii_case("Static") {
                params.append_pair(&key, &value);
            }
        }
    }

    params.append_pair("MediaSourceId", source_id);
    params.append_pair("Static", "true");
    if !has_api_key && !has_token {
        params.append_pair("api_key", "");
    }

    let query = params.finish();
    format!("/videos/{item_id}/stream?{query}")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn parses_item_ids_from_video_and_item_paths() {
        assert_eq!(parse_item_id("/videos/abc/stream").as_deref(), Some("abc"));
        assert_eq!(
            parse_item_id("/Items/xyz/PlaybackInfo").as_deref(),
            Some("xyz")
        );
    }

    #[test]
    fn detects_strm_media_source() {
        assert!(is_strm_media_source(
            &json!({"IsRemote": true, "IsInfiniteStream": false})
        ));
        assert!(!is_strm_media_source(
            &json!({"IsRemote": true, "IsInfiniteStream": true})
        ));
        assert!(!is_strm_media_source(&json!({"IsRemote": false})));
    }

    #[test]
    fn patches_system_info_to_proxy_address() {
        let patched = patch_system_info(
            json!({
                "HttpServerPortNumber": 8096,
                "WebSocketPortNumber": 8096,
                "LocalAddress": "http://10.0.0.10:8096",
                "LocalAddresses": ["http://10.0.0.10:8096"],
                "RemoteAddresses": ["https://example.com:8920"]
            }),
            8097,
            "http://panel.lan:8097",
        );

        assert_eq!(patched["HttpServerPortNumber"], 8097);
        assert_eq!(patched["WebSocketPortNumber"], 8097);
        assert_eq!(patched["LocalAddress"], "http://panel.lan:8097");
        assert_eq!(patched["LocalAddresses"], json!(["http://panel.lan:8097"]));
        assert_eq!(patched["RemoteAddresses"], json!(["http://panel.lan:8097"]));
    }

    #[test]
    fn patches_playback_info_for_strm_sources() {
        let body = json!({
            "MediaSources": [{
                "Id": "source1",
                "IsRemote": true,
                "IsInfiniteStream": false,
                "SupportsTranscoding": true,
                "TranscodingUrl": "/transcode"
            }]
        });

        let patched = patch_playback_info(body, "/Items/item1/PlaybackInfo", Some("DeviceId=d1"));
        let source = &patched["MediaSources"][0];

        assert_eq!(source["SupportsDirectPlay"], true);
        assert_eq!(source["SupportsDirectStream"], true);
        assert_eq!(source["SupportsTranscoding"], false);
        assert!(source.get("TranscodingUrl").is_none());
        assert_eq!(
            source["DirectStreamUrl"].as_str(),
            Some("/videos/item1/stream?DeviceId=d1&MediaSourceId=source1&Static=true&api_key=")
        );
    }
}
