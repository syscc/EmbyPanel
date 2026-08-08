use serde::{Deserialize, Serialize};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use crate::{
    config::Config,
    error::{AppError, AppResult},
    ip_location::IpLocation,
};

fn emby_api_url(config: &Config, path: &str) -> AppResult<url::Url> {
    let mut url = config.emby_url(path)?;
    url.query_pairs_mut()
        .append_pair("api_key", &config.emby_api_key);
    Ok(url)
}

async fn send_checked(
    request: reqwest::RequestBuilder,
    operation: &str,
) -> AppResult<reqwest::Response> {
    let response = request.send().await?;
    if !response.status().is_success() {
        return Err(AppError::BadGateway(format!(
            "{operation} failed with status {}",
            response.status()
        )));
    }
    Ok(response)
}

#[derive(Debug, Deserialize)]
struct ItemsResponse {
    #[serde(rename = "Items", default)]
    items: Vec<EmbyItem>,
}

#[derive(Debug, Deserialize)]
struct EmbyItem {
    #[serde(rename = "MediaSources", default)]
    media_sources: Vec<MediaSource>,
}

#[derive(Debug, Deserialize)]
struct MediaSource {
    #[serde(rename = "Id")]
    id: Option<String>,
    #[serde(rename = "Path")]
    path: Option<String>,
}

pub async fn get_media_path(
    client: &reqwest::Client,
    config: &Config,
    item_id: &str,
    media_source_id: Option<&str>,
    user_agent: &str,
) -> AppResult<Option<String>> {
    let mut url = emby_api_url(config, "/Items")?;
    url.query_pairs_mut()
        .append_pair("Ids", item_id)
        .append_pair("Fields", "Path,MediaSources");

    let mut request = client.get(url);
    if !user_agent.trim().is_empty() {
        request = request.header(reqwest::header::USER_AGENT, user_agent);
    }
    let response = send_checked(request, "Emby item query").await?;

    let data = response.json::<ItemsResponse>().await?;
    let Some(item) = data.items.first() else {
        return Err(AppError::BadGateway("Emby item not found".to_string()));
    };

    let source = if let Some(media_source_id) = media_source_id {
        item.media_sources
            .iter()
            .find(|source| source.id.as_deref() == Some(media_source_id))
    } else {
        item.media_sources.first()
    };

    Ok(source.and_then(|source| source.path.clone()))
}

#[derive(Debug, Clone, Serialize)]
pub struct PlaybackSession {
    pub server_id: String,
    pub server_name: String,
    pub id: String,
    pub item_id: String,
    pub series_id: Option<String>,
    pub season_number: Option<i32>,
    pub episode_number: Option<i32>,
    pub item_type: Option<String>,
    pub media_source_id: Option<String>,
    #[serde(skip_serializing)]
    pub(crate) device_id: Option<String>,
    pub user_name: String,
    pub client: String,
    pub device_name: String,
    pub user_agent: String,
    pub playback_ip: Option<String>,
    pub ip_location: Option<IpLocation>,
    pub item_name: String,
    pub series_name: Option<String>,
    pub position_ticks: Option<i64>,
    pub runtime_ticks: Option<i64>,
    pub percent: Option<u8>,
    pub play_method: Option<String>,
    pub playback_mode: String,
    pub transcoding: bool,
}

#[derive(Debug, Serialize)]
pub struct MediaOverview {
    pub server_id: String,
    pub movie_count: i64,
    pub series_count: i64,
    pub episode_count: i64,
    pub user_count: i64,
    pub server_name: String,
    pub version: String,
    pub operating_system: String,
    pub library_count: i64,
}

#[derive(Debug, Deserialize)]
struct ItemCountsResponse {
    #[serde(rename = "MovieCount", default)]
    movie_count: i64,
    #[serde(rename = "SeriesCount", default)]
    series_count: i64,
    #[serde(rename = "EpisodeCount", default)]
    episode_count: i64,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum UsersResponse {
    Array(Vec<serde_json::Value>),
    Object {
        #[serde(rename = "Items", alias = "items", default)]
        items: Vec<serde_json::Value>,
    },
}

impl UsersResponse {
    fn into_values(self) -> Vec<serde_json::Value> {
        match self {
            Self::Array(items) | Self::Object { items } => items,
        }
    }
}

#[derive(Debug, Deserialize)]
struct UserResponse {
    #[serde(rename = "Name")]
    name: Option<String>,
    #[serde(rename = "Policy")]
    policy: Option<serde_json::Value>,
}

/// Fetches the raw user objects needed by the management API. The caller is
/// responsible for projecting these values into a response so Emby secrets or
/// unrelated policy fields are never exposed to the browser.
pub async fn list_users(
    client: &reqwest::Client,
    config: &Config,
) -> AppResult<Vec<serde_json::Value>> {
    let mut url = emby_api_url(config, "/Users")?;
    url.query_pairs_mut()
        .append_pair("Fields", "Policy")
        .append_pair("Limit", "1000")
        .append_pair("SortBy", "Name");
    let response = send_checked(client.get(url), "Emby users query").await?;
    Ok(response.json::<UsersResponse>().await?.into_values())
}

pub async fn create_user(
    client: &reqwest::Client,
    config: &Config,
    name: &str,
) -> AppResult<serde_json::Value> {
    let url = emby_api_url(config, "/Users/New")?;
    let response = send_checked(
        client
            .post(url)
            .json(&serde_json::json!({ "Name": name.trim() })),
        "Emby user creation",
    )
    .await?;
    Ok(response.json().await?)
}

pub async fn delete_user(
    client: &reqwest::Client,
    config: &Config,
    user_id: &str,
) -> AppResult<()> {
    let user_id = validate_user_id(user_id)?;
    let url = emby_api_url(config, &format!("/Users/{user_id}"))?;
    send_checked(client.delete(url), "Emby user deletion").await?;
    Ok(())
}

pub async fn set_user_password(
    client: &reqwest::Client,
    config: &Config,
    user_id: &str,
    current_password: &str,
    new_password: &str,
) -> AppResult<()> {
    let user_id = validate_user_id(user_id)?;
    let url = emby_api_url(config, &format!("/Users/{user_id}/Password"))?;
    send_checked(
        client.post(url).json(&serde_json::json!({
            "CurrentPw": current_password,
            "NewPw": new_password,
            "ResetPassword": false,
        })),
        "Emby user password update",
    )
    .await?;
    Ok(())
}

pub async fn get_user_profile_value(
    client: &reqwest::Client,
    config: &Config,
    user_id: &str,
) -> AppResult<serde_json::Value> {
    let user_id = validate_user_id(user_id)?;
    let mut url = emby_api_url(config, &format!("/Users/{user_id}"))?;
    url.query_pairs_mut().append_pair("Fields", "Policy");
    let response = send_checked(client.get(url), "Emby user query").await?;
    Ok(response.json().await?)
}

pub async fn update_user_policy_value(
    client: &reqwest::Client,
    config: &Config,
    user_id: &str,
    policy: &serde_json::Value,
) -> AppResult<()> {
    let user_id = validate_user_id(user_id)?;
    let url = emby_api_url(config, &format!("/Users/{user_id}/Policy"))?;
    send_checked(client.post(url).json(policy), "Emby user policy update").await?;
    Ok(())
}

pub async fn reset_user_password(
    client: &reqwest::Client,
    config: &Config,
    user_id: &str,
    new_password: &str,
) -> AppResult<()> {
    let user_id = validate_user_id(user_id)?;
    let profile = get_user_profile_value(client, config, user_id).await?;
    let mut original_policy = profile
        .get("Policy")
        .or_else(|| profile.get("policy"))
        .filter(|value| value.is_object())
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    let was_disabled = original_policy
        .get("IsDisabled")
        .or_else(|| original_policy.get("is_disabled"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    original_policy
        .as_object_mut()
        .expect("user policy was normalized to an object")
        .insert("IsDisabled".to_string(), serde_json::json!(was_disabled));
    let mut disabled_policy = original_policy.clone();
    disabled_policy
        .as_object_mut()
        .expect("user policy was normalized to an object")
        .insert("IsDisabled".to_string(), serde_json::json!(true));
    update_user_policy_value(client, config, user_id, &disabled_policy).await?;

    let mut url = config.emby_url(&format!("/Users/{user_id}/Password"))?;
    url.query_pairs_mut()
        .append_pair("api_key", &config.emby_api_key);
    let response = client
        .post(url.clone())
        .json(&serde_json::json!({
            "CurrentPw": "",
            "NewPw": "",
            "ResetPassword": true,
        }))
        .send()
        .await?;
    if !response.status().is_success() {
        let status = response.status();
        update_user_policy_value(client, config, user_id, &original_policy)
            .await
            .map_err(|error| {
                AppError::BadGateway(format!(
                    "Emby user password reset failed with status {status}, and restoring the original user policy also failed: {}",
                    error.safe_log_message()
                ))
            })?;
        return Err(AppError::BadGateway(format!(
            "Emby user password reset failed with status {status}"
        )));
    }

    let response = client
        .post(url)
        .json(&serde_json::json!({
            "CurrentPw": "",
            "NewPw": new_password,
            "ResetPassword": false,
        }))
        .send()
        .await?;
    if !response.status().is_success() {
        return Err(AppError::BadGateway(format!(
            "Emby password was cleared, but setting the replacement password failed with status {}; the user remains disabled",
            response.status()
        )));
    }

    update_user_policy_value(client, config, user_id, &original_policy)
        .await
        .map_err(|error| {
            AppError::BadGateway(format!(
                "Emby password was changed, but restoring the original user policy failed; the user remains disabled: {}",
                error.safe_log_message()
            ))
        })?;
    Ok(())
}

pub async fn list_sessions_value(
    client: &reqwest::Client,
    config: &Config,
) -> AppResult<Vec<serde_json::Value>> {
    let url = emby_api_url(config, "/Sessions")?;
    let response = send_checked(client.get(url), "Emby sessions query").await?;
    Ok(response.json().await?)
}

pub async fn list_virtual_folders_value(
    client: &reqwest::Client,
    config: &Config,
) -> AppResult<Vec<serde_json::Value>> {
    let url = emby_api_url(config, "/Library/VirtualFolders")?;
    let response = send_checked(client.get(url), "Emby virtual folders query").await?;
    Ok(response.json().await?)
}

fn validate_user_id(user_id: &str) -> AppResult<&str> {
    let user_id = user_id.trim();
    if user_id.is_empty()
        || user_id.len() > 128
        || !user_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(AppError::Validation("invalid Emby user id".to_string()));
    }
    Ok(user_id)
}

#[derive(Debug, Deserialize)]
struct SystemInfoResponse {
    #[serde(rename = "ServerName")]
    server_name: Option<String>,
    #[serde(rename = "Version")]
    version: Option<String>,
    #[serde(rename = "OperatingSystem")]
    operating_system: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ViewsResponse {
    #[serde(rename = "Items", default)]
    items: Vec<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct SessionResponse {
    #[serde(rename = "Id")]
    id: Option<String>,
    #[serde(rename = "UserName")]
    user_name: Option<String>,
    #[serde(rename = "Client")]
    client: Option<String>,
    #[serde(rename = "DeviceName")]
    device_name: Option<String>,
    #[serde(rename = "DeviceId")]
    device_id: Option<String>,
    #[serde(rename = "ApplicationVersion")]
    application_version: Option<String>,
    #[serde(
        rename = "RemoteEndPoint",
        alias = "RemoteEndpoint",
        alias = "RemoteAddress",
        alias = "IpAddress"
    )]
    remote_endpoint: Option<String>,
    #[serde(rename = "NowPlayingItem")]
    now_playing_item: Option<NowPlayingItem>,
    #[serde(rename = "PlayState")]
    play_state: Option<PlayState>,
    #[serde(rename = "TranscodingInfo")]
    transcoding_info: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct NowPlayingItem {
    #[serde(rename = "Id")]
    id: Option<String>,
    #[serde(rename = "SeriesId")]
    series_id: Option<String>,
    #[serde(rename = "Name")]
    name: Option<String>,
    #[serde(rename = "SeriesName")]
    series_name: Option<String>,
    #[serde(rename = "ParentIndexNumber")]
    season_number: Option<i32>,
    #[serde(rename = "IndexNumber")]
    episode_number: Option<i32>,
    #[serde(rename = "Type")]
    item_type: Option<String>,
    #[serde(rename = "RunTimeTicks")]
    runtime_ticks: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct PlayState {
    #[serde(rename = "PositionTicks")]
    position_ticks: Option<i64>,
    #[serde(rename = "PlayMethod")]
    play_method: Option<String>,
    #[serde(rename = "MediaSourceId")]
    media_source_id: Option<String>,
}

pub async fn get_active_playback_sessions(
    client: &reqwest::Client,
    config: &Config,
) -> AppResult<Vec<PlaybackSession>> {
    let url = emby_api_url(config, "/Sessions")?;
    let response = send_checked(client.get(url), "Emby sessions query").await?;

    let sessions = response.json::<Vec<SessionResponse>>().await?;
    let (server_id, server_name) = playback_server_label(config);
    Ok(sessions
        .into_iter()
        .filter_map(|session| {
            let item = session.now_playing_item?;
            let play_state = session.play_state;
            let runtime_ticks = item.runtime_ticks;
            let position_ticks = play_state.as_ref().and_then(|state| state.position_ticks);
            let play_method = play_state
                .as_ref()
                .and_then(|state| state.play_method.clone());
            let media_source_id = play_state
                .as_ref()
                .and_then(|state| state.media_source_id.clone());
            let device_id = session.device_id.clone();
            let transcoding = session.transcoding_info.is_some()
                || play_method
                    .as_deref()
                    .is_some_and(|method| method.eq_ignore_ascii_case("Transcode"));
            let playback_mode = emby_playback_mode(play_method.as_deref(), transcoding).to_string();
            Some(PlaybackSession {
                server_id: server_id.clone(),
                server_name: server_name.clone(),
                id: session.id.unwrap_or_default(),
                item_id: item.id.unwrap_or_default(),
                series_id: item.series_id,
                season_number: item.season_number,
                episode_number: item.episode_number,
                item_type: item.item_type,
                media_source_id,
                device_id,
                user_name: session.user_name.unwrap_or_else(|| "Unknown".to_string()),
                client: session
                    .client
                    .clone()
                    .unwrap_or_else(|| "Unknown".to_string()),
                device_name: session.device_name.unwrap_or_else(|| "Unknown".to_string()),
                user_agent: session_user_agent(
                    session.client.as_deref(),
                    session.application_version.as_deref(),
                    session.device_id.as_deref(),
                ),
                playback_ip: session_remote_ip(session.remote_endpoint.as_deref()),
                ip_location: None,
                item_name: item.name.unwrap_or_else(|| "Unknown".to_string()),
                series_name: item.series_name,
                position_ticks,
                runtime_ticks,
                percent: playback_percent(position_ticks, runtime_ticks),
                play_method,
                playback_mode,
                transcoding,
            })
        })
        .collect())
}

fn emby_playback_mode(play_method: Option<&str>, transcoding: bool) -> &'static str {
    if transcoding || play_method.is_some_and(|method| method.eq_ignore_ascii_case("Transcode")) {
        return "transcode";
    }

    match play_method.map(str::trim) {
        Some(method) if method.eq_ignore_ascii_case("DirectPlay") => "emby_direct_play",
        Some(method) if method.eq_ignore_ascii_case("DirectStream") => "emby_direct_stream",
        _ => "unknown",
    }
}

pub async fn get_user_name_by_device_id(
    client: &reqwest::Client,
    config: &Config,
    device_id: &str,
) -> AppResult<Option<String>> {
    let device_id = device_id.trim();
    if device_id.is_empty() {
        return Ok(None);
    }

    let url = emby_api_url(config, "/Sessions")?;
    let response = send_checked(client.get(url), "Emby sessions query").await?;

    let sessions = response.json::<Vec<SessionResponse>>().await?;
    Ok(sessions
        .into_iter()
        .find(|session| session.device_id.as_deref() == Some(device_id))
        .and_then(|session| session.user_name)
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty()))
}

pub async fn get_user_name_by_user_id(
    client: &reqwest::Client,
    config: &Config,
    user_id: &str,
) -> AppResult<Option<String>> {
    let user_id = user_id.trim();
    if user_id.is_empty() {
        return Ok(None);
    }

    let url = emby_api_url(config, &format!("/Users/{user_id}"))?;
    let response = send_checked(client.get(url), "Emby user query").await?;

    Ok(response
        .json::<UserResponse>()
        .await?
        .name
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty()))
}

pub async fn get_user_name_by_token(
    client: &reqwest::Client,
    config: &Config,
    token: &str,
) -> AppResult<Option<String>> {
    let token = token.trim();
    if token.is_empty() {
        return Ok(None);
    }

    let url = config.emby_url("/Users/Me")?;
    let response = client.get(url).header("X-Emby-Token", token).send().await?;
    if !response.status().is_success() {
        return Ok(None);
    }

    Ok(response
        .json::<UserResponse>()
        .await?
        .name
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty()))
}

pub async fn find_user_by_name(
    client: &reqwest::Client,
    config: &Config,
    user_name: &str,
) -> AppResult<Option<UserLookup>> {
    let user_name = user_name.trim();
    if user_name.is_empty() {
        return Ok(None);
    }

    let url = emby_api_url(config, "/Users/Query")?;
    let response = send_checked(client.get(url), "Emby users query").await?;

    let users = response.json::<UsersResponse>().await?;
    for value in users.into_values() {
        if let Some(found) = user_lookup_from_value(&value, user_name) {
            return Ok(Some(found));
        }
    }
    Ok(None)
}

pub async fn set_user_disabled(
    client: &reqwest::Client,
    config: &Config,
    user_id: &str,
    disabled: bool,
) -> AppResult<()> {
    let user_id = user_id.trim();
    if user_id.is_empty() {
        return Ok(());
    }

    let mut current = get_user_profile(client, config, user_id).await?;
    let mut policy = current
        .policy
        .take()
        .unwrap_or_else(|| serde_json::json!({}));
    if !policy.is_object() {
        policy = serde_json::json!({});
    }
    if let Some(object) = policy.as_object_mut() {
        object.insert("IsDisabled".to_string(), serde_json::json!(disabled));
    }

    let url = emby_api_url(config, &format!("/Users/{user_id}/Policy"))?;
    send_checked(client.post(url).json(&policy), "Emby user policy update").await?;
    Ok(())
}

fn playback_server_label(config: &Config) -> (String, String) {
    config
        .servers
        .first()
        .map(|server| (server.id.clone(), server.name.clone()))
        .unwrap_or_else(|| ("default".to_string(), "默认服务器".to_string()))
}

fn user_lookup_from_value(value: &serde_json::Value, user_name: &str) -> Option<UserLookup> {
    let name = value.get("Name")?.as_str()?.trim();
    if !name.eq_ignore_ascii_case(user_name) {
        return None;
    }
    let id = value.get("Id")?.as_str()?.trim();
    if id.is_empty() {
        return None;
    }
    Some(UserLookup { id: id.to_string() })
}

async fn get_user_profile(
    client: &reqwest::Client,
    config: &Config,
    user_id: &str,
) -> AppResult<UserResponse> {
    let url = emby_api_url(config, &format!("/Users/{user_id}"))?;
    let response = send_checked(client.get(url), "Emby user query").await?;
    Ok(response.json::<UserResponse>().await?)
}

#[derive(Debug, Clone)]
pub struct UserLookup {
    pub id: String,
}

fn session_user_agent(
    client: Option<&str>,
    application_version: Option<&str>,
    device_id: Option<&str>,
) -> String {
    let client = client.unwrap_or("Unknown").trim();
    let version = application_version.unwrap_or("").trim();
    let device_id = device_id.unwrap_or("").trim();
    if !version.is_empty() {
        format!("{client}/{version}")
    } else if !device_id.is_empty() {
        format!("{client} ({device_id})")
    } else {
        client.to_string()
    }
}

fn session_remote_ip(remote_endpoint: Option<&str>) -> Option<String> {
    let endpoint = remote_endpoint?.trim();
    if endpoint.is_empty() {
        return None;
    }

    if let Ok(socket_addr) = endpoint.parse::<SocketAddr>() {
        return Some(normalize_ip(socket_addr.ip()));
    }
    if let Ok(ip) = endpoint.parse::<IpAddr>() {
        return Some(normalize_ip(ip));
    }

    if endpoint.starts_with('[') {
        if let Some(end) = endpoint.find(']') {
            let ip = &endpoint[1..end];
            if let Ok(ip) = ip.parse::<IpAddr>() {
                return Some(normalize_ip(ip));
            }
        }
    }

    if let Some((ip, _port)) = endpoint.rsplit_once(':') {
        if let Ok(ip) = ip.parse::<Ipv4Addr>() {
            return Some(ip.to_string());
        }
    }

    None
}

fn normalize_ip(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(ip) => ip.to_string(),
        IpAddr::V6(ip) => ip
            .to_ipv4_mapped()
            .map_or_else(|| ip.to_string(), |ip| ip.to_string()),
    }
}

pub async fn get_media_overview(
    client: &reqwest::Client,
    config: &Config,
) -> AppResult<MediaOverview> {
    let counts = get_item_counts(client, config).await?;
    let user_count = get_user_count(client, config).await.unwrap_or_default();
    let info = get_system_info(client, config)
        .await
        .unwrap_or(SystemInfoResponse {
            server_name: None,
            version: None,
            operating_system: None,
        });
    let library_count = get_library_count(client, config).await.unwrap_or_default();

    Ok(MediaOverview {
        server_id: config
            .servers
            .first()
            .map(|server| server.id.clone())
            .unwrap_or_else(|| "default".to_string()),
        movie_count: counts.movie_count,
        series_count: counts.series_count,
        episode_count: counts.episode_count,
        user_count,
        server_name: info
            .server_name
            .unwrap_or_else(|| "Emby Server".to_string()),
        version: info.version.unwrap_or_else(|| "--".to_string()),
        operating_system: info.operating_system.unwrap_or_else(|| "--".to_string()),
        library_count,
    })
}

pub async fn validate_connection(client: &reqwest::Client, config: &Config) -> AppResult<()> {
    let _ = get_system_info(client, config).await?;
    Ok(())
}

async fn get_item_counts(
    client: &reqwest::Client,
    config: &Config,
) -> AppResult<ItemCountsResponse> {
    let url = emby_api_url(config, "/Items/Counts")?;
    let response = send_checked(client.get(url), "Emby item counts query").await?;
    Ok(response.json::<ItemCountsResponse>().await?)
}

async fn get_user_count(client: &reqwest::Client, config: &Config) -> AppResult<i64> {
    let url = emby_api_url(config, "/Users/Query")?;
    let response = send_checked(client.get(url), "Emby users query").await?;
    let users = response.json::<UsersResponse>().await?;
    Ok(match users {
        UsersResponse::Array(items) => items.len() as i64,
        UsersResponse::Object { items } => items.len() as i64,
    })
}

async fn get_system_info(
    client: &reqwest::Client,
    config: &Config,
) -> AppResult<SystemInfoResponse> {
    let url = emby_api_url(config, "/System/Info")?;
    let response = send_checked(client.get(url), "Emby system info query").await?;
    Ok(response.json::<SystemInfoResponse>().await?)
}

async fn get_library_count(client: &reqwest::Client, config: &Config) -> AppResult<i64> {
    let url = emby_api_url(config, "/Library/VirtualFolders/Query")?;
    let response = send_checked(client.get(url), "Emby library query").await?;
    let views = response.json::<ViewsResponse>().await?;
    Ok(views.items.len() as i64)
}

fn playback_percent(position_ticks: Option<i64>, runtime_ticks: Option<i64>) -> Option<u8> {
    let position_ticks = position_ticks?;
    let runtime_ticks = runtime_ticks?;
    if runtime_ticks <= 0 {
        return None;
    }
    Some(((position_ticks as f64 / runtime_ticks as f64) * 100.0).clamp(0.0, 100.0) as u8)
}

#[cfg(test)]
mod tests {
    use super::{emby_api_url, emby_playback_mode};
    use crate::config::Config;

    #[test]
    fn api_url_adds_key_without_dropping_path() {
        let mut config = Config::default_runtime();
        config.emby_host = "http://emby.example.test/base".to_string();
        config.emby_api_key = "secret key".to_string();

        let url = emby_api_url(&config, "/Users").expect("valid Emby URL");
        assert_eq!(url.path(), "/Users");
        assert_eq!(
            url.query_pairs()
                .find(|(key, _)| key == "api_key")
                .map(|(_, value)| value.into_owned()),
            Some("secret key".to_string())
        );
    }

    #[test]
    fn playback_mode_normalizes_emby_methods_and_transcoding() {
        assert_eq!(
            emby_playback_mode(Some("DirectPlay"), false),
            "emby_direct_play"
        );
        assert_eq!(
            emby_playback_mode(Some("DirectStream"), false),
            "emby_direct_stream"
        );
        assert_eq!(emby_playback_mode(Some("Transcode"), false), "transcode");
        assert_eq!(emby_playback_mode(Some("DirectPlay"), true), "transcode");
        assert_eq!(emby_playback_mode(None, false), "unknown");
    }
}
