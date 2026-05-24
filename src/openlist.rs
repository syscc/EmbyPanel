use serde::{Deserialize, Serialize};
use url::Url;

use crate::{
    config::Config,
    error::{AppError, AppResult},
};

#[derive(Debug, Serialize)]
struct FsGetRequest<'a> {
    path: &'a str,
}

#[derive(Debug, Deserialize)]
struct FsGetResponse {
    code: Option<i64>,
    message: Option<String>,
    data: Option<FsGetData>,
}

#[derive(Debug, Deserialize)]
struct FsGetData {
    raw_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FsListResponse {
    code: Option<i64>,
    message: Option<String>,
}

pub async fn fs_get(
    client: &reqwest::Client,
    config: &Config,
    path: &str,
    user_agent: &str,
) -> AppResult<Option<String>> {
    let url = config.openlist_url("/api/fs/get")?;
    let response = client
        .post(url)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .header(reqwest::header::USER_AGENT, user_agent)
        .header(
            reqwest::header::AUTHORIZATION,
            config.openlist_token.as_deref().unwrap_or_default(),
        )
        .json(&FsGetRequest { path })
        .send()
        .await?;

    if !response.status().is_success() {
        tracing::error!(status = %response.status(), path, "OpenList HTTP request failed");
        return Ok(None);
    }

    let data = response.json::<FsGetResponse>().await?;
    if data.code != Some(200) {
        tracing::error!(
            code = ?data.code,
            message = ?data.message,
            path,
            "OpenList API returned failure"
        );
        return Ok(None);
    }

    Ok(data.data.and_then(|data| data.raw_url))
}

pub async fn validate_connection(client: &reqwest::Client, config: &Config) -> AppResult<()> {
    let url = config.openlist_url("/api/fs/list")?;
    let response = client
        .post(url)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .header(
            reqwest::header::AUTHORIZATION,
            config.openlist_token.as_deref().unwrap_or_default(),
        )
        .json(&serde_json::json!({
            "path": "/",
            "password": "",
            "refresh": false,
            "page": 1,
            "per_page": 1,
        }))
        .send()
        .await?;

    if !response.status().is_success() {
        return Err(AppError::BadGateway(format!(
            "OpenList HTTP request failed with status {}",
            response.status()
        )));
    }

    let data = response.json::<FsListResponse>().await?;
    if data.code != Some(200) {
        return Err(AppError::BadGateway(format!(
            "OpenList API returned failure: {}",
            data.message.unwrap_or_else(|| "unknown error".to_string())
        )));
    }
    Ok(())
}

pub fn extract_openlist_path(http_url: &str) -> Option<String> {
    let url = Url::parse(http_url).ok()?;
    let path = url.path();
    let raw_path = path.strip_prefix("/d/")?;
    urlencoding::decode(&format!("/{raw_path}"))
        .ok()
        .map(|value| value.into_owned())
}

pub fn cache_key(item_id: &str, media_source_id: Option<&str>, user_agent: &str) -> String {
    format!(
        "{}:{}:{}",
        item_id,
        media_source_id.unwrap_or("default"),
        user_agent
    )
}

pub fn ensure_raw_url(value: Option<String>) -> AppResult<String> {
    let value =
        value.ok_or_else(|| AppError::BadGateway("OpenList did not return raw_url".to_string()))?;
    let url = Url::parse(&value)?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(AppError::BadGateway(
            "OpenList raw_url must use http or https".to_string(),
        ));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_and_decodes_openlist_path() {
        let path = extract_openlist_path("http://openlist.local/d/movie/%E6%B5%8B%E8%AF%95.mkv");
        assert_eq!(path.as_deref(), Some("/movie/测试.mkv"));
    }

    #[test]
    fn rejects_non_openlist_download_path() {
        assert_eq!(
            extract_openlist_path("http://openlist.local/file/a.mkv"),
            None
        );
    }

    #[test]
    fn cache_key_uses_default_source() {
        assert_eq!(cache_key("abc", None, "ua"), "abc:default:ua".to_string());
    }

    #[test]
    fn ensure_raw_url_requires_http_or_https() {
        assert!(ensure_raw_url(Some("https://cdn.example.test/a.mkv".to_string())).is_ok());
        assert!(ensure_raw_url(Some("file:///etc/passwd".to_string())).is_err());
        assert!(ensure_raw_url(Some("/relative/path".to_string())).is_err());
    }
}
