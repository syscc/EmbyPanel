use serde::{Deserialize, Serialize};
use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::OnceLock,
};

use crate::{
    AppState,
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

async fn send_response(
    request: reqwest::RequestBuilder,
    operation: &str,
) -> AppResult<reqwest::Response> {
    send_response_observed(request, operation, &mut Default::default()).await
}

async fn send_response_observed(
    request: reqwest::RequestBuilder,
    operation: &str,
    observation: &mut crate::diagnostics::UpstreamObservation,
) -> AppResult<reqwest::Response> {
    let started = std::time::Instant::now();
    let result =
        crate::diagnostics::network::observe_send(observation, async { request.send().await })
            .await;
    result
        .map(|response| {
            observation.response_headers_ms = Some(started.elapsed().as_millis() as u64);
            observation.status = Some(response.status().as_u16());
            response
        })
        .map_err(|err| {
            observation.error_kind = Some(crate::error::reqwest_error_kind(&err).to_string());
            AppError::BadGateway(format!(
                "{operation} failed: {}",
                crate::error::safe_reqwest_error_message(&err)
            ))
        })
}

fn upstream_status_error(operation: &str, status: reqwest::StatusCode) -> AppError {
    // Never read or include an upstream error body: it can contain credentials.
    AppError::BadGateway(format!("{operation} failed with status {status}"))
}

async fn send_checked(
    request: reqwest::RequestBuilder,
    operation: &str,
) -> AppResult<reqwest::Response> {
    send_checked_observed(request, operation, &mut Default::default()).await
}

async fn send_checked_observed(
    request: reqwest::RequestBuilder,
    operation: &str,
    observation: &mut crate::diagnostics::UpstreamObservation,
) -> AppResult<reqwest::Response> {
    let response = send_response_observed(request, operation, observation).await?;
    if !response.status().is_success() {
        observation.error_kind = Some("upstream_http".to_string());
        return Err(upstream_status_error(operation, response.status()));
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
    #[serde(rename = "Type")]
    item_type: Option<String>,
    #[serde(rename = "MediaSources", default)]
    media_sources: Vec<MediaSource>,
    #[serde(rename = "Name")]
    name: Option<String>,
    #[serde(rename = "SeriesName")]
    series_name: Option<String>,
    #[serde(rename = "ParentIndexNumber")]
    season_number: Option<i32>,
    #[serde(rename = "IndexNumber")]
    episode_number: Option<i32>,
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
    Ok(get_media_path_observed(
        client,
        config,
        item_id,
        media_source_id,
        user_agent,
        &mut Default::default(),
    )
    .await?
    .path)
}

/// Media labels come from the same Items request; callers must not fetch on cache hits.
#[derive(Debug, Default)]
pub(crate) struct MediaPathResolution {
    pub item_type: Option<String>,
    pub path: Option<String>,
    pub media_name: Option<String>,
    pub series_name: Option<String>,
    pub season_number: Option<i32>,
    pub episode_number: Option<i32>,
    pub media_source_id: Option<String>,
}

pub(crate) async fn get_media_path_observed(
    client: &reqwest::Client,
    config: &Config,
    item_id: &str,
    media_source_id: Option<&str>,
    user_agent: &str,
    observation: &mut crate::diagnostics::UpstreamObservation,
) -> AppResult<MediaPathResolution> {
    let mut url = emby_api_url(config, "/Items")?;
    url.query_pairs_mut()
        .append_pair("Ids", item_id)
        .append_pair(
            "Fields",
            "Type,Path,MediaSources,SeriesName,ParentIndexNumber,IndexNumber",
        );

    let mut request = client.get(url);
    if !user_agent.trim().is_empty() {
        request = request.header(reqwest::header::USER_AGENT, user_agent);
    }
    let response = send_checked_observed(request, "Emby item query", observation).await?;
    let started = std::time::Instant::now();
    let data = response.json::<ItemsResponse>().await;
    observation.response_body_ms = Some(started.elapsed().as_millis() as u64);
    let data = data.map_err(|err| {
        observation.error_kind = Some(crate::error::reqwest_error_kind(&err).to_string());
        AppError::from(err)
    })?;
    let Some(item) = data.items.first() else {
        observation.error_kind = Some("not_found".to_string());
        return Err(AppError::BadGateway("Emby item not found".to_string()));
    };

    let source = if let Some(media_source_id) = media_source_id {
        item.media_sources
            .iter()
            .find(|source| source.id.as_deref() == Some(media_source_id))
    } else {
        item.media_sources.first()
    };

    Ok(MediaPathResolution {
        item_type: item.item_type.clone(),
        path: source.and_then(|source| source.path.clone()),
        media_source_id: source.and_then(|source| source.id.clone()),
        media_name: item.name.clone(),
        series_name: item.series_name.clone(),
        season_number: item.season_number,
        episode_number: item.episode_number,
    })
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
    pub device_id: Option<String>,
    pub user_name: String,
    pub client: String,
    pub device_name: String,
    #[serde(skip_serializing_if = "String::is_empty")]
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
    pub bitrate: Option<u64>,
    pub bandwidth: Option<u64>,
    pub transcode_reason: Option<String>,
    pub application_version: Option<String>,
    pub remote_endpoint: Option<String>,
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
    let response =
        crate::diagnostics::network::send(client.post(url.clone()).json(&serde_json::json!({
            "CurrentPw": "",
            "NewPw": "",
            "ResetPassword": true,
        })))
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

    let response = crate::diagnostics::network::send(client.post(url).json(&serde_json::json!({
        "CurrentPw": "",
        "NewPw": new_password,
        "ResetPassword": false,
    })))
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

#[derive(Debug, Deserialize)]
struct VirtualFolderQueryValueResult {
    #[serde(rename = "Items", alias = "items", default)]
    items: Vec<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum VirtualFolderQueryResponse {
    Wrapped(VirtualFolderQueryValueResult),
    LegacyArray(Vec<serde_json::Value>),
}

impl VirtualFolderQueryResponse {
    fn into_items(self) -> Vec<serde_json::Value> {
        match self {
            Self::Wrapped(result) => result.items,
            Self::LegacyArray(items) => items,
        }
    }
}

pub async fn list_virtual_folders_value(
    client: &reqwest::Client,
    config: &Config,
) -> AppResult<Vec<serde_json::Value>> {
    let url = emby_api_url(config, "/Library/VirtualFolders/Query")?;
    let response = send_checked(client.get(url), "Emby virtual folders query").await?;
    let result = response
        .json::<VirtualFolderQueryResponse>()
        .await
        .map_err(|_| AppError::BadGateway("invalid virtual folder response".to_string()))?;
    Ok(result.into_items())
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
    #[serde(rename = "UserAgent")]
    user_agent: Option<String>,
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
            let device_id = non_empty_session_value(session.device_id.clone());
            let application_version = non_empty_session_value(session.application_version.clone());
            let remote_endpoint = non_empty_session_value(session.remote_endpoint.clone());
            let (bitrate, bandwidth, transcode_reason) =
                session_transcoding_details(session.transcoding_info.as_ref());
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
                // Emby /Sessions does not guarantee a UserAgent field. Never
                // synthesize one from client/version/device metadata: those are
                // separate fields, not an HTTP user-agent value.
                user_agent: non_empty_session_value(session.user_agent).unwrap_or_default(),
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
                bitrate,
                bandwidth,
                transcode_reason,
                application_version,
                remote_endpoint,
            })
        })
        .collect())
}

fn non_empty_session_value(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let value = value.trim();
        (!value.is_empty()).then(|| value.to_string())
    })
}

fn positive_u64_field(info: &serde_json::Value, names: &[&str]) -> Option<u64> {
    names.iter().find_map(|name| {
        info.get(*name)
            .and_then(serde_json::Value::as_u64)
            .filter(|value| *value > 0)
    })
}

fn transcode_reasons(info: &serde_json::Value) -> Option<String> {
    let value = info
        .get("TranscodeReasons")
        .or_else(|| info.get("TranscodeReason"))?;
    match value {
        serde_json::Value::String(reason) => non_empty_session_value(Some(reason.clone())),
        serde_json::Value::Array(reasons) => {
            let reasons = reasons
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|reason| !reason.is_empty())
                .collect::<Vec<_>>();
            (!reasons.is_empty()).then(|| reasons.join(", "))
        }
        _ => None,
    }
}

fn session_transcoding_details(
    info: Option<&serde_json::Value>,
) -> (Option<u64>, Option<u64>, Option<String>) {
    let Some(info) = info.filter(|value| value.is_object()) else {
        return (None, None, None);
    };

    // Emby's aggregate Bitrate is the least ambiguous stream rate. Only fall
    // back to the component rates when the aggregate value is absent.
    let bitrate = positive_u64_field(info, &["Bitrate"]);
    let bitrate = bitrate.or_else(|| {
        let video = positive_u64_field(info, &["VideoBitrate"]);
        let audio = positive_u64_field(info, &["AudioBitrate"]);
        video.and_then(|video| audio.and_then(|audio| video.checked_add(audio)))
    });
    // Do not treat Throttle (a boolean state) as a bandwidth value. Some
    // Emby versions expose an explicit Bandwidth field; otherwise it stays
    // absent rather than reporting the target bitrate twice.
    let bandwidth = positive_u64_field(info, &["Bandwidth"]);
    let reason = transcode_reasons(info);
    (bitrate, bandwidth, reason)
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
    let response =
        crate::diagnostics::network::send(client.get(url).header("X-Emby-Token", token)).await?;
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

fn deserialize_optional_string<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(value.and_then(|value| value.as_str().map(str::to_string)))
}

fn deserialize_string_vec<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(match value {
        Some(serde_json::Value::Array(values)) => values
            .into_iter()
            .filter_map(|value| value.as_str().map(str::to_string))
            .collect(),
        _ => Vec::new(),
    })
}

#[derive(Debug, Serialize, Deserialize)]
pub struct VirtualFolder {
    #[serde(
        rename = "Id",
        alias = "id",
        default,
        deserialize_with = "deserialize_optional_string"
    )]
    pub id: Option<String>,
    #[serde(
        rename = "ItemId",
        alias = "itemId",
        default,
        deserialize_with = "deserialize_optional_string"
    )]
    pub item_id: Option<String>,
    #[serde(
        rename = "Name",
        alias = "name",
        default,
        deserialize_with = "deserialize_optional_string"
    )]
    pub name: Option<String>,
    #[serde(
        rename = "Locations",
        alias = "locations",
        default,
        deserialize_with = "deserialize_string_vec"
    )]
    pub locations: Vec<String>,
    #[serde(
        rename = "CollectionType",
        alias = "collectionType",
        default,
        deserialize_with = "deserialize_optional_string"
    )]
    pub collection_type: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ScheduledTask {
    #[serde(rename = "Id")]
    pub id: String,
    #[serde(rename = "Name")]
    pub name: String,
    #[serde(rename = "Description", default)]
    pub description: Option<String>,
    #[serde(rename = "Category", default)]
    pub category: Option<String>,
    #[serde(rename = "Key", default)]
    pub key: Option<String>,
    #[serde(rename = "State")]
    pub state: String,
    #[serde(rename = "IsHidden", default)]
    pub is_hidden: Option<bool>,
    #[serde(rename = "IsEnabled", default)]
    pub is_enabled: Option<bool>,
    #[serde(rename = "Triggers", default)]
    pub triggers: Vec<serde_json::Value>,
    #[serde(rename = "CurrentProgressPercentage", default)]
    pub current_progress_percentage: Option<f64>,
    #[serde(rename = "LastExecutionResult", default)]
    pub last_execution_result: Option<TaskExecutionResult>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TaskExecutionResult {
    #[serde(rename = "Status", default)]
    pub status: Option<String>,
    #[serde(rename = "Name", default)]
    pub name: Option<String>,
    #[serde(rename = "Key", default)]
    pub key: Option<String>,
    #[serde(rename = "Id", default)]
    pub id: Option<String>,
    #[serde(rename = "ErrorMessage", default)]
    pub error_message: Option<String>,
    #[serde(rename = "LongErrorMessage", default)]
    pub long_error_message: Option<String>,
    #[serde(rename = "StartTimeUtc", default)]
    pub start_time_utc: Option<String>,
    #[serde(rename = "EndTimeUtc", default)]
    pub end_time_utc: Option<String>,
}

#[derive(Serialize)]
struct ValidatePathRequest<'a> {
    #[serde(rename = "ValidateWriteable")]
    validate_writeable: bool,
    #[serde(rename = "IsFile")]
    is_file: Option<bool>,
    #[serde(rename = "Username")]
    username: &'a str,
    #[serde(rename = "Password")]
    password: &'a str,
}

pub async fn get_config_for_server(state: &AppState, server_id: &str) -> AppResult<Option<Config>> {
    let server_id = server_id.trim();
    let config = state.config.read().await;
    if config.servers.is_empty() {
        if server_id == "default" {
            Ok(Some(config.clone()))
        } else {
            Ok(None)
        }
    } else if let Some(server) = config.servers.iter().find(|server| server.id == server_id) {
        Ok(Some(
            config.proxy_config_for_server(Some(server.id.as_str())),
        ))
    } else {
        Ok(None)
    }
}

pub async fn list_virtual_folders(
    client: &reqwest::Client,
    config: &Config,
) -> AppResult<Vec<VirtualFolder>> {
    let values = list_virtual_folders_value(client, config).await?;
    values
        .into_iter()
        .map(|value| {
            serde_json::from_value(value)
                .map_err(|_| AppError::BadGateway("invalid virtual folder response".to_string()))
        })
        .collect()
}

/// Ask the Emby server to validate a path visible from its own filesystem.
/// Credentials are sent only in the JSON body and are never formatted into errors.
pub async fn validate_server_path(
    client: &reqwest::Client,
    config: &Config,
    path: &str,
    validate_writeable: bool,
    is_file: Option<bool>,
    username: Option<&str>,
    password: Option<&str>,
) -> AppResult<()> {
    if path.trim().is_empty() || path.chars().any(char::is_control) {
        return Err(AppError::Validation("invalid Emby server path".to_string()));
    }
    let mut url = emby_api_url(config, "/Environment/ValidatePath")?;
    url.query_pairs_mut().append_pair("Path", path);
    let body = ValidatePathRequest {
        validate_writeable,
        is_file,
        username: username.unwrap_or(""),
        password: password.unwrap_or(""),
    };
    send_checked(
        client.post(url).json(&body),
        "Emby server-side path validation",
    )
    .await?;
    Ok(())
}

pub async fn list_scheduled_tasks(
    client: &reqwest::Client,
    config: &Config,
) -> AppResult<Vec<ScheduledTask>> {
    let url = emby_api_url(config, "/ScheduledTasks")?;
    let response = send_checked(client.get(url), "Emby scheduled tasks").await?;
    Ok(response.json::<Vec<ScheduledTask>>().await?)
}

pub async fn start_scheduled_task(
    client: &reqwest::Client,
    config: &Config,
    id: &str,
) -> AppResult<()> {
    let id = validate_scheduled_task_id(id)?;
    let url = emby_api_url(config, &format!("/ScheduledTasks/Running/{id}"))?;
    send_checked(client.post(url), "Emby start scheduled task").await?;
    Ok(())
}

pub async fn cancel_scheduled_task(
    client: &reqwest::Client,
    config: &Config,
    id: &str,
) -> AppResult<()> {
    let id = validate_scheduled_task_id(id)?;
    let primary_url = emby_api_url(config, &format!("/ScheduledTasks/Running/{id}/Delete"))?;
    let primary = send_response(client.post(primary_url), "Emby cancel scheduled task").await?;
    if primary.status().is_success() {
        return Ok(());
    }
    if matches!(
        primary.status(),
        reqwest::StatusCode::NOT_FOUND | reqwest::StatusCode::METHOD_NOT_ALLOWED
    ) {
        let fallback_url = emby_api_url(config, &format!("/ScheduledTasks/Running/{id}"))?;
        send_checked(
            client.delete(fallback_url),
            "Emby cancel scheduled task fallback",
        )
        .await?;
        return Ok(());
    }
    Err(upstream_status_error(
        "Emby cancel scheduled task",
        primary.status(),
    ))
}

fn validate_scheduled_task_id(id: &str) -> AppResult<String> {
    let id = id.trim();
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(AppError::Validation(
            "invalid scheduled task id".to_string(),
        ));
    }
    Ok(id.to_string())
}

const TASK_ERROR_SUMMARY_MAX_BYTES: usize = 256;

pub(crate) fn sanitize_task_error_summary(message: &str) -> String {
    static URL_PATTERN: OnceLock<regex::Regex> = OnceLock::new();
    let url_pattern = URL_PATTERN
        .get_or_init(|| regex::Regex::new(r"(?i)https?://\S+").expect("task error URL regex"));
    let flattened = message
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect::<String>();
    // Signed URLs have provider-specific query names; omit the entire query rather
    // than assuming every upstream uses one of our known credential keys.
    let redacted_urls = url_pattern.replace_all(&flattened, |captures: &regex::Captures<'_>| {
        let Ok(mut url) = url::Url::parse(&captures[0]) else {
            return "[REDACTED URL]".to_string();
        };
        let _ = url.set_username("");
        let _ = url.set_password(None);
        url.set_query(None);
        url.set_fragment(None);
        url.to_string()
    });
    let redacted = crate::diagnostics::sanitize_diagnostic_text(&redacted_urls);
    let normalized = redacted.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut summary = String::new();
    let mut truncated = false;
    for ch in normalized.chars() {
        if summary.len() + ch.len_utf8() > TASK_ERROR_SUMMARY_MAX_BYTES {
            truncated = true;
            break;
        }
        summary.push(ch);
    }
    if truncated {
        while summary.len() + '…'.len_utf8() > TASK_ERROR_SUMMARY_MAX_BYTES {
            summary.pop();
        }
        summary.push('…');
    }
    summary
}

pub fn task_failure_summary(task: &ScheduledTask) -> Option<String> {
    let result = task.last_execution_result.as_ref()?;
    if !result.status.as_deref().is_some_and(|status| {
        matches!(
            status.trim().to_ascii_lowercase().as_str(),
            "failed" | "cancelled" | "aborted"
        )
    }) {
        return None;
    }
    let message = result
        .error_message
        .as_deref()
        .filter(|message| !message.trim().is_empty())
        .or_else(|| {
            result
                .long_error_message
                .as_deref()
                .filter(|message| !message.trim().is_empty())
        })
        .unwrap_or("");
    let summary = sanitize_task_error_summary(message);
    if summary.is_empty() {
        Some("unknown error".to_string())
    } else {
        Some(summary)
    }
}

pub async fn get_scheduled_task(
    client: &reqwest::Client,
    config: &Config,
    id: &str,
) -> AppResult<ScheduledTask> {
    let id = validate_scheduled_task_id(id)?;
    let url = emby_api_url(config, &format!("/ScheduledTasks/{id}"))?;
    let response = send_checked(client.get(url), "Emby scheduled task").await?;
    response
        .json()
        .await
        .map_err(|_| AppError::BadGateway("invalid scheduled task response".to_string()))
}

pub async fn get_task_failure_summary(
    client: &reqwest::Client,
    config: &Config,
    id: &str,
) -> AppResult<Option<String>> {
    let task = get_scheduled_task(client, config, id).await?;
    Ok(task_failure_summary(&task))
}

fn validate_session_id(session_id: &str) -> AppResult<&str> {
    let session_id = session_id.trim();
    if session_id.is_empty()
        || session_id.len() > 100
        || !session_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(AppError::Validation("invalid session id".to_string()));
    }
    Ok(session_id)
}

fn validate_session_message(message: &str) -> AppResult<&str> {
    let message = message.trim();
    if message.is_empty() || message.len() > 255 || message.chars().any(|c| c.is_control()) {
        return Err(AppError::Validation("invalid session message".to_string()));
    }
    Ok(message)
}

pub async fn stop_session(
    client: &reqwest::Client,
    config: &Config,
    session_id: &str,
) -> AppResult<()> {
    let session_id = validate_session_id(session_id)?;
    let url = emby_api_url(config, &format!("/Sessions/{session_id}/Playing/Stop"))?;
    send_checked(
        client
            .post(url)
            .json(&serde_json::json!({"Command": "Stop"})),
        "Emby stop session",
    )
    .await?;
    Ok(())
}

pub async fn send_session_message(
    client: &reqwest::Client,
    config: &Config,
    session_id: &str,
    message: &str,
) -> AppResult<()> {
    let session_id = validate_session_id(session_id)?;
    let message = validate_session_message(message)?;
    let mut url = emby_api_url(config, &format!("/Sessions/{session_id}/Message"))?;
    url.query_pairs_mut()
        .append_pair("Text", message)
        .append_pair("Header", "EmbyPanel");
    send_checked(client.post(url), "Emby send session message").await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        ScheduledTask, TaskExecutionResult, VirtualFolder, emby_api_url, emby_playback_mode,
        get_active_playback_sessions, get_task_failure_summary, playback_percent,
        session_transcoding_details, task_failure_summary, validate_scheduled_task_id,
        validate_session_id, validate_session_message,
    };
    use crate::config::Config;
    use crate::error::AppError;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn sample_task(status: Option<&str>, error_message: &str) -> ScheduledTask {
        ScheduledTask {
            id: "task-1".to_string(),
            name: "Scan".to_string(),
            description: Some("LibraryScan".to_string()),
            category: None,
            key: None,
            state: "Idle".to_string(),
            is_hidden: None,
            is_enabled: None,
            triggers: Vec::new(),
            current_progress_percentage: None,
            last_execution_result: status.map(|status| TaskExecutionResult {
                status: Some(status.to_string()),
                name: None,
                key: None,
                id: None,
                error_message: Some(error_message.to_string()),
                long_error_message: None,
                start_time_utc: None,
                end_time_utc: None,
            }),
        }
    }

    async fn serve_one_http_response(
        status_line: &str,
        body: &str,
    ) -> (String, tokio::task::JoinHandle<String>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback");
        let addr = listener.local_addr().expect("local addr");
        let status_line = status_line.to_string();
        let body = body.to_string();
        let handle = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            let mut buf = vec![0_u8; 8192];
            let n = stream.read(&mut buf).await.expect("read request");
            let request = String::from_utf8_lossy(&buf[..n]).into_owned();
            let response = format!(
                "{status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream
                .write_all(response.as_bytes())
                .await
                .expect("write response");
            request
        });
        (format!("http://{addr}"), handle)
    }

    async fn serve_http_responses(
        responses: Vec<(&str, &str)>,
    ) -> (String, tokio::task::JoinHandle<Vec<String>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback");
        let addr = listener.local_addr().expect("local addr");
        let responses = responses
            .into_iter()
            .map(|(status, body)| (status.to_string(), body.to_string()))
            .collect::<Vec<_>>();
        let handle = tokio::spawn(async move {
            let mut requests = Vec::with_capacity(responses.len());
            for (status_line, body) in responses {
                let (mut stream, _) = listener.accept().await.expect("accept");
                let mut buf = vec![0_u8; 16 * 1024];
                let n = stream.read(&mut buf).await.expect("read request");
                requests.push(String::from_utf8_lossy(&buf[..n]).into_owned());
                let response = format!(
                    "{status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream
                    .write_all(response.as_bytes())
                    .await
                    .expect("write response");
            }
            requests
        });
        (format!("http://{addr}"), handle)
    }

    #[tokio::test]
    async fn playback_sessions_map_only_upstream_user_agent() {
        let body = r#"[
            {
                "Id": "session-real-ua",
                "UserName": "alice",
                "Client": "Emby Web",
                "DeviceName": "Browser",
                "DeviceId": "device-real",
                "ApplicationVersion": "4.8.0",
                "UserAgent": "Mozilla/5.0 (X11; Linux x86_64)",
                "NowPlayingItem": {"Id": "item-real", "Name": "Real UA", "RunTimeTicks": 1000},
                "PlayState": {"PositionTicks": 500, "PlayMethod": "DirectPlay"}
            },
            {
                "Id": "session-no-ua",
                "UserName": "bob",
                "Client": "Fabricated Client",
                "DeviceName": "Device",
                "DeviceId": "device-no-ua",
                "ApplicationVersion": "9.9.9",
                "NowPlayingItem": {"Id": "item-no-ua", "Name": "No UA", "RunTimeTicks": 1000},
                "PlayState": {"PositionTicks": 250, "PlayMethod": "DirectPlay"}
            }
        ]"#;
        let (base, handle) = serve_one_http_response("HTTP/1.1 200 OK", body).await;
        let mut config = Config::default_runtime();
        config.emby_host = base;
        config.emby_api_key = "test-key".to_string();

        let sessions = get_active_playback_sessions(&reqwest::Client::new(), &config)
            .await
            .expect("playback sessions");
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].user_agent, "Mozilla/5.0 (X11; Linux x86_64)");
        assert_eq!(
            serde_json::to_value(&sessions[0]).expect("serialize real ua")["user_agent"],
            "Mozilla/5.0 (X11; Linux x86_64)"
        );
        assert!(sessions[1].user_agent.is_empty());
        assert!(
            serde_json::to_value(&sessions[1])
                .expect("serialize missing ua")
                .get("user_agent")
                .is_none()
        );
        assert!(!sessions[1].user_agent.contains("Fabricated Client"));
        assert!(!sessions[1].user_agent.contains("9.9.9"));
        assert!(!sessions[1].user_agent.contains("device-no-ua"));
        assert_eq!(sessions[1].percent, Some(25));

        let request = handle.await.expect("server task");
        assert!(request.starts_with("GET /Sessions?"));
    }

    #[tokio::test]
    async fn virtual_folder_query_unwraps_items_and_preserves_optional_fields() {
        let body = r#"{"Items":[{"Id":"folder-1","ItemId":"item-1","Name":"Movies","Locations":["/one","/two"],"CollectionType":"movies","Unknown":true}],"TotalRecordCount":1}"#;
        let (base, handle) = serve_one_http_response("HTTP/1.1 200 OK", body).await;
        let mut config = Config::default_runtime();
        config.emby_host = base;
        config.emby_api_key = "test-key".to_string();
        let folders = super::list_virtual_folders(&reqwest::Client::new(), &config)
            .await
            .expect("folders");
        assert_eq!(folders.len(), 1);
        assert_eq!(folders[0].id.as_deref(), Some("folder-1"));
        assert_eq!(folders[0].item_id.as_deref(), Some("item-1"));
        assert_eq!(folders[0].locations, vec!["/one", "/two"]);
        assert_eq!(folders[0].collection_type.as_deref(), Some("movies"));
        let request = handle.await.expect("server task");
        assert!(request.starts_with("GET /Library/VirtualFolders/Query?"));
    }

    #[tokio::test]
    async fn validate_server_path_uses_emby_endpoint_and_never_returns_body() {
        let (base, handle) = serve_one_http_response("HTTP/1.1 200 OK", "not-json").await;
        let mut config = Config::default_runtime();
        config.emby_host = base;
        config.emby_api_key = "test-key".to_string();
        super::validate_server_path(
            &reqwest::Client::new(),
            &config,
            "/server/media",
            true,
            Some(false),
            Some("fake-user"),
            Some("fake-password"),
        )
        .await
        .expect("validation ok");
        let request = handle.await.expect("server task");
        assert!(request.starts_with("POST /Environment/ValidatePath?"));
        assert!(request.contains("Path=%2Fserver%2Fmedia"));
        assert!(request.contains("\"ValidateWriteable\":true"));
        assert!(request.contains("\"IsFile\":false"));
        assert!(request.contains("\"Username\":\"fake-user\""));
        assert!(request.contains("\"Password\":\"fake-password\""));
    }

    #[tokio::test]
    async fn validate_server_path_redacts_upstream_error_body() {
        let (base, handle) =
            serve_one_http_response("HTTP/1.1 500 Internal Server Error", "token=do-not-leak")
                .await;
        let mut config = Config::default_runtime();
        config.emby_host = base;
        config.emby_api_key = "test-key".to_string();
        let error = super::validate_server_path(
            &reqwest::Client::new(),
            &config,
            "/server/media",
            false,
            Some(false),
            None,
            None,
        )
        .await
        .expect_err("validation should fail");
        assert_eq!(
            error.safe_log_message(),
            "Emby server-side path validation failed with status 500 Internal Server Error"
        );
        assert!(!error.safe_log_message().contains("do-not-leak"));
        let request = handle.await.expect("server task");
        assert!(request.starts_with("POST /Environment/ValidatePath?"));
        assert!(!request.contains("do-not-leak"));
    }

    #[tokio::test]
    async fn scheduled_task_actions_use_current_methods_and_narrow_cancel_fallback() {
        let (base, handle) = serve_http_responses(vec![
            ("HTTP/1.1 404 Not Found", "{}"),
            ("HTTP/1.1 204 No Content", ""),
        ])
        .await;
        let mut config = Config::default_runtime();
        config.emby_host = base;
        config.emby_api_key = "test-key".to_string();
        let client = reqwest::Client::new();
        super::cancel_scheduled_task(&client, &config, "task-1")
            .await
            .expect("cancel fallback ok");
        let requests = handle.await.expect("server task");
        assert!(requests[0].starts_with("POST /ScheduledTasks/Running/task-1/Delete?"));
        assert!(requests[1].starts_with("DELETE /ScheduledTasks/Running/task-1?"));

        let (base, handle) = serve_one_http_response("HTTP/1.1 204 No Content", "").await;
        config.emby_host = base;
        super::start_scheduled_task(&client, &config, "task-1")
            .await
            .expect("start ok");
        let request = handle.await.expect("server task");
        assert!(request.starts_with("POST /ScheduledTasks/Running/task-1?"));
    }

    #[tokio::test]
    async fn cancel_does_not_fallback_for_forbidden_upstream() {
        let (base, handle) = serve_one_http_response("HTTP/1.1 403 Forbidden", "secret body").await;
        let mut config = Config::default_runtime();
        config.emby_host = base;
        config.emby_api_key = "test-key".to_string();
        let error = super::cancel_scheduled_task(&reqwest::Client::new(), &config, "task-1")
            .await
            .expect_err("forbidden");
        assert_eq!(
            error.safe_log_message(),
            "Emby cancel scheduled task failed with status 403 Forbidden"
        );
        handle.await.expect("server task");
    }

    #[test]
    fn task_failure_summary_redacts_secrets_and_truncates_upstream_text() {
        let long = format!(
            "api_key=SECRET password=PW Bearer TOKEN {}",
            "x".repeat(400)
        );
        let summary = super::task_failure_summary(&sample_task(Some("Failed"), &long))
            .expect("failure summary");
        assert!(!summary.contains("SECRET"));
        assert!(!summary.contains("PW"));
        assert!(!summary.contains("TOKEN"));
        assert!(summary.len() <= 257);
    }

    #[test]
    fn task_failure_summary_removes_url_credentials_and_provider_signatures() {
        let message = concat!(
            "下载失败 https://operator:USERINFO_CANARY@upstream.invalid/movie?",
            "sign=SIGN_CANARY&X-Amz-Credential=AWS_CANARY#FRAGMENT_CANARY ",
            "Authorization: Basic BASIC_CANARY 密码错误"
        );
        let summary = task_failure_summary(&sample_task(Some("Failed"), message)).unwrap();
        for secret in [
            "operator",
            "USERINFO_CANARY",
            "SIGN_CANARY",
            "AWS_CANARY",
            "FRAGMENT_CANARY",
            "BASIC_CANARY",
        ] {
            assert!(
                !summary.contains(secret),
                "credential remained in task summary"
            );
        }
        assert!(summary.contains("upstream.invalid/movie"));
        assert!(summary.contains("下载失败"));
        assert_eq!(super::sanitize_task_error_summary(&summary), summary);
        assert!(summary.len() <= super::TASK_ERROR_SUMMARY_MAX_BYTES);
    }

    #[test]
    fn task_failure_redaction_keeps_field_variants_and_complete_url_boundaries() {
        for message in [
            "api-key=API_CANARY access-token=ACCESS_CANARY credential=CRED_CANARY",
            "https://host.invalid/movie?part=1,2&sign=QUERY_CANARY",
            "https://user:prefix,PASS_CANARY@host.invalid/movie",
            "https://host.invalid/movie?part='quoted'&sign=QUOTE_CANARY",
        ] {
            let summary = super::sanitize_task_error_summary(message);
            assert!(
                !summary.contains("CANARY"),
                "sensitive task data was not redacted"
            );
            assert_eq!(super::sanitize_task_error_summary(&summary), summary);
        }
    }

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

    #[test]
    fn playback_percent_clamps_and_handles_zeroes() {
        assert_eq!(playback_percent(None, None), None);
        assert_eq!(playback_percent(Some(0), Some(100)), Some(0));
        assert_eq!(playback_percent(Some(50), Some(100)), Some(50));
        assert_eq!(playback_percent(Some(100), Some(100)), Some(100));
        assert_eq!(playback_percent(Some(150), Some(100)), Some(100));
        assert_eq!(playback_percent(Some(0), Some(0)), None);
        assert_eq!(playback_percent(Some(100), Some(0)), None);
    }

    #[test]
    fn session_transcoding_details_maps_rates_and_reason_array() {
        let info = serde_json::json!({
            "Bitrate": 8_000_000,
            "AudioBitrate": 192_000,
            "VideoBitrate": 7_500_000,
            "Throttle": true,
            "TranscodeReasons": ["VideoCodecNotSupported", "  AudioCodecNotSupported ", ""],
        });
        assert_eq!(
            session_transcoding_details(Some(&info)),
            (
                Some(8_000_000),
                None,
                Some("VideoCodecNotSupported, AudioCodecNotSupported".to_string())
            )
        );
    }

    #[test]
    fn session_transcoding_details_falls_back_to_component_bitrate_and_handles_missing_values() {
        let info = serde_json::json!({
            "AudioBitrate": 192_000,
            "VideoBitrate": 7_500_000,
            "Bandwidth": 9_000_000,
            "TranscodeReasons": "ContainerNotSupported",
        });
        assert_eq!(
            session_transcoding_details(Some(&info)),
            (
                Some(7_692_000),
                Some(9_000_000),
                Some("ContainerNotSupported".to_string())
            )
        );
        assert_eq!(session_transcoding_details(None), (None, None, None));
        assert_eq!(
            session_transcoding_details(Some(&serde_json::json!({"Throttle": true}))),
            (None, None, None)
        );
    }

    #[test]
    fn virtual_folder_serde_deserializes_pascal_case_and_defaults() {
        let json = r#"{"Id":"1","Name":"Movies","IsVisible":true,"Locations":["path1"],"LocationTypes":["movies"],"CollectionType":"movies","Extra":"field"}"#;
        let folder: VirtualFolder = serde_json::from_str(json).unwrap();
        assert_eq!(folder.id.as_deref(), Some("1"));
        assert_eq!(folder.name.as_deref(), Some("Movies"));
        assert_eq!(folder.locations, vec!["path1"]);
        assert_eq!(folder.collection_type.as_deref(), Some("movies"));
        // extra fields like LocationTypes ignored by serde
    }

    #[test]
    fn scheduled_task_serde_deserializes_and_ignores_unmapped_fields() {
        let json = r#"{"Id":"task1","Name":"Task","Description":"desc","State":"Running","LastExecutionResult":{"Status":"Failed","ErrorMessage":"err"},"CurrentProgressPercentage":50,"Extra":"more"}"#;
        let task: ScheduledTask = serde_json::from_str(json).unwrap();
        assert_eq!(task.id, "task1");
        assert_eq!(task.name, "Task");
        assert_eq!(task.state, "Running");
        assert_eq!(task.description.as_deref(), Some("desc"));
        assert_eq!(task.current_progress_percentage, Some(50.0));
    }

    #[test]
    fn scheduled_task_id_validation_edge_cases() {
        assert!(validate_scheduled_task_id("").is_err());
        assert!(validate_scheduled_task_id("a-b_c").is_ok());
        assert!(validate_scheduled_task_id(&"a".repeat(129)).is_err());
        assert!(validate_scheduled_task_id("a@b").is_err());
    }

    #[test]
    fn session_id_validation_edge_cases() {
        assert!(validate_session_id("").is_err());
        assert!(validate_session_id("sess-123").is_ok());
        assert!(validate_session_id(&"a".repeat(101)).is_err());
        assert!(validate_session_id("a@b").is_err());
    }

    #[test]
    fn session_message_validation_edge_cases() {
        assert!(validate_session_message("").is_err());
        assert!(validate_session_message("msg").is_ok());
        assert!(validate_session_message(&"a".repeat(256)).is_err());
        assert!(validate_session_message("msg\x00").is_err());
    }

    async fn serve_session_contract(
        status_line: &str,
        path: &str,
        message: Option<&str>,
    ) -> (Config, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind session mock");
        let addr = listener.local_addr().expect("session mock address");
        let mut config = Config::default_runtime();
        config.emby_host = format!("http://{addr}");
        config.emby_api_key = "local-session-test-key".to_string();
        let key = config.emby_api_key.clone();
        let status_line = status_line.to_string();
        let path = path.to_string();
        let message = message.map(str::to_string);
        let handle = tokio::spawn(async move {
            let (mut stream, _) =
                tokio::time::timeout(std::time::Duration::from_secs(3), listener.accept())
                    .await
                    .expect("session request timeout")
                    .expect("accept session request");
            let mut request = Vec::new();
            let (header_end, content_length) = loop {
                let mut chunk = [0_u8; 1024];
                let n = stream.read(&mut chunk).await.expect("read session request");
                assert!(n > 0, "session request ended before headers");
                request.extend_from_slice(&chunk[..n]);
                assert!(request.len() <= 16 * 1024, "session request too large");
                if let Some(end) = request.windows(4).position(|part| part == b"\r\n\r\n") {
                    let headers = std::str::from_utf8(&request[..end]).expect("request headers");
                    let length = headers
                        .lines()
                        .filter_map(|line| line.split_once(':'))
                        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                        .map(|(_, value)| value.trim().parse::<usize>().expect("body length"))
                        .unwrap_or(0);
                    assert!(length <= 1024, "session request body too large");
                    break (end + 4, length);
                }
            };
            // Read the complete body; one TCP read need not contain the JSON.
            while request.len() < header_end + content_length {
                let mut chunk = [0_u8; 1024];
                let n = stream.read(&mut chunk).await.expect("read session body");
                assert!(n > 0, "session request ended before body");
                request.extend_from_slice(&chunk[..n]);
            }
            let headers = std::str::from_utf8(&request[..header_end]).expect("request headers");
            let parts = headers
                .lines()
                .next()
                .expect("request line")
                .split_whitespace()
                .collect::<Vec<_>>();
            assert_eq!(parts.len(), 3);
            assert_eq!(parts[0], "POST");
            assert_eq!(parts[2], "HTTP/1.1");
            let url =
                url::Url::parse(&format!("http://localhost{}", parts[1])).expect("request target");
            assert_eq!(url.path(), path);
            let mut query = url.query_pairs().into_owned().collect::<Vec<_>>();
            query.sort();
            let mut expected_query = vec![("api_key".to_string(), key)];
            if let Some(text) = message.as_ref() {
                expected_query.push(("Text".to_string(), text.clone()));
                expected_query.push(("Header".to_string(), "EmbyPanel".to_string()));
            }
            expected_query.sort();
            // Do not print key/message values if the query contract fails.
            assert!(query == expected_query, "session query contract mismatch");
            let body = &request[header_end..];
            assert_eq!(body.len(), content_length);
            if message.is_some() {
                assert!(
                    body.is_empty(),
                    "message must use query parameters, not JSON"
                );
            } else {
                let json: serde_json::Value = serde_json::from_slice(body).expect("stop JSON");
                assert_eq!(json, serde_json::json!({"Command": "Stop"}));
                assert!(
                    headers
                        .lines()
                        .any(|line| line.eq_ignore_ascii_case("content-type: application/json"))
                );
            }
            let body = if status_line.contains("204") {
                ""
            } else {
                "mock upstream error body"
            };
            let response = format!(
                "{status_line}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream
                .write_all(response.as_bytes())
                .await
                .expect("write session response");
        });
        (config, handle)
    }

    #[tokio::test]
    async fn session_stop_contract_uses_playing_stop_and_safe_failure() {
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(std::time::Duration::from_secs(3))
            .build()
            .expect("local client");
        for status in [
            "HTTP/1.1 204 No Content",
            "HTTP/1.1 500 Internal Server Error",
        ] {
            let (config, handle) =
                serve_session_contract(status, "/Sessions/session-1/Playing/Stop", None).await;
            let result = super::stop_session(&client, &config, " session-1 ").await;
            handle.await.expect("stop contract assertions");
            if status.contains("204") {
                result.expect("stop succeeds");
            } else {
                let error = result.expect_err("stop fails");
                assert!(matches!(error, AppError::BadGateway(_)));
                let expected = "Emby stop session failed with status 500 Internal Server Error";
                assert_eq!(error.safe_log_message(), expected);
                assert_eq!(error.to_string(), expected);
            }
        }
    }

    #[tokio::test]
    async fn session_message_contract_uses_text_header_query_and_safe_failure() {
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(std::time::Duration::from_secs(3))
            .build()
            .expect("local client");
        let text = "本地测试 & + ? # / = %";
        for status in [
            "HTTP/1.1 204 No Content",
            "HTTP/1.1 500 Internal Server Error",
        ] {
            let (config, handle) =
                serve_session_contract(status, "/Sessions/session-1/Message", Some(text)).await;
            let result = super::send_session_message(
                &client,
                &config,
                " session-1 ",
                &format!("  {text}  "),
            )
            .await;
            handle.await.expect("message contract assertions");
            if status.contains("204") {
                result.expect("message succeeds");
            } else {
                let error = result.expect_err("message fails");
                assert!(matches!(error, AppError::BadGateway(_)));
                let expected =
                    "Emby send session message failed with status 500 Internal Server Error";
                assert_eq!(error.safe_log_message(), expected);
                assert_eq!(error.to_string(), expected);
            }
        }
    }

    #[test]
    fn task_failure_summary_failed_only_and_empty_error() {
        assert_eq!(
            task_failure_summary(&sample_task(Some("Failed"), "disk full")),
            Some("disk full".to_string())
        );
        assert_eq!(
            task_failure_summary(&sample_task(Some("failed"), "  boom  ")),
            Some("boom".to_string())
        );
        assert_eq!(
            task_failure_summary(&sample_task(Some("FAILED"), "")),
            Some("unknown error".to_string())
        );
        assert_eq!(
            task_failure_summary(&sample_task(Some(" Failed "), "   ")),
            Some("unknown error".to_string())
        );
    }

    #[test]
    fn task_failure_summary_ignores_non_failed_and_missing_result() {
        assert_eq!(
            task_failure_summary(&sample_task(Some("Completed"), "should ignore")),
            None
        );
        assert_eq!(
            task_failure_summary(&sample_task(Some("Running"), "still running")),
            None
        );
        assert_eq!(task_failure_summary(&sample_task(None, "")), None);
        // mixed/abnormal: empty status after defaulting is not failed
        assert_eq!(task_failure_summary(&sample_task(Some(""), "err")), None);
    }

    #[test]
    fn task_failure_summary_handles_missing_fields_and_abnormal_json() {
        let missing_error = r#"{"Id":"t1","Name":"N","Description":"d","State":"Idle","LastExecutionResult":{"Status":"Failed"}}"#;
        let task: ScheduledTask = serde_json::from_str(missing_error).unwrap();
        assert_eq!(
            task_failure_summary(&task),
            Some("unknown error".to_string())
        );

        let missing_status = r#"{"Id":"t1","Name":"N","Description":"d","State":"Idle","LastExecutionResult":{"ErrorMessage":"x"}}"#;
        let task: ScheduledTask = serde_json::from_str(missing_status).unwrap();
        assert_eq!(task_failure_summary(&task), None);

        let missing_result = r#"{"Id":"t1","Name":"N","Description":"d","State":"Idle"}"#;
        let task: ScheduledTask = serde_json::from_str(missing_result).unwrap();
        assert_eq!(task_failure_summary(&task), None);

        // required task identity fields still rejected
        assert!(serde_json::from_str::<ScheduledTask>(r#"{"Name":"N"}"#).is_err());
        assert!(serde_json::from_str::<ScheduledTask>("[]").is_err());
        assert!(serde_json::from_str::<ScheduledTask>("null").is_err());
    }

    #[tokio::test]
    async fn get_task_failure_summary_loopback_failed_only() {
        let body = r#"{"Id":"task-1","Name":"Scan","Description":"LibraryScan","State":"Idle","LastExecutionResult":{"Status":"Failed","ErrorMessage":"disk full"}}"#;
        let (base, handle) = serve_one_http_response("HTTP/1.1 200 OK", body).await;

        let mut config = Config::default_runtime();
        config.emby_host = base;
        config.emby_api_key = "secret-test-key".to_string();
        let client = reqwest::Client::new();

        let summary = get_task_failure_summary(&client, &config, "task-1")
            .await
            .expect("summary ok");
        assert_eq!(summary.as_deref(), Some("disk full"));

        let request = handle.await.expect("server task");
        assert!(request.contains("GET /ScheduledTasks/task-1?"));
        assert!(request.contains("api_key=secret-test-key"));
    }

    #[tokio::test]
    async fn get_task_failure_summary_loopback_non_failed_and_empty_error() {
        let completed = r#"{"Id":"task-1","Name":"Scan","Description":"LibraryScan","State":"Idle","LastExecutionResult":{"Status":"Completed","ErrorMessage":"nope"}}"#;
        let (base, handle) = serve_one_http_response("HTTP/1.1 200 OK", completed).await;
        let mut config = Config::default_runtime();
        config.emby_host = base;
        config.emby_api_key = "secret-test-key".to_string();
        let client = reqwest::Client::new();
        let summary = get_task_failure_summary(&client, &config, "task-1")
            .await
            .expect("summary ok");
        assert_eq!(summary, None);
        handle.await.expect("server task");

        let empty_failed = r#"{"Id":"task-1","Name":"Scan","Description":"LibraryScan","State":"Idle","LastExecutionResult":{"Status":"Failed","ErrorMessage":"  "}}"#;
        let (base, handle) = serve_one_http_response("HTTP/1.1 200 OK", empty_failed).await;
        config.emby_host = base;
        let summary = get_task_failure_summary(&client, &config, "task-1")
            .await
            .expect("summary ok");
        assert_eq!(summary.as_deref(), Some("unknown error"));
        handle.await.expect("server task");
    }

    #[tokio::test]
    async fn get_task_failure_summary_loopback_status_and_json_errors_are_safe() {
        let secret = "secret-test-key";
        let (base, handle) = serve_one_http_response(
            "HTTP/1.1 500 Internal Server Error",
            "api_key=should-not-leak body",
        )
        .await;
        let mut config = Config::default_runtime();
        config.emby_host = base;
        config.emby_api_key = secret.to_string();
        let client = reqwest::Client::new();

        let err = get_task_failure_summary(&client, &config, "task-1")
            .await
            .expect_err("status error");
        let msg = err.safe_log_message();
        assert!(matches!(err, AppError::BadGateway(_)));
        assert!(msg.contains("failed with status 500"));
        assert!(!msg.contains(secret));
        assert!(!msg.contains("should-not-leak"));
        assert!(!format!("{err}").contains(secret));
        handle.await.expect("server task");

        let (base, handle) = serve_one_http_response("HTTP/1.1 200 OK", "not-json").await;
        config.emby_host = base;
        let err = get_task_failure_summary(&client, &config, "task-1")
            .await
            .expect_err("json error");
        let msg = err.safe_log_message();
        assert_eq!(msg, "invalid scheduled task response");
        assert!(!msg.contains(secret));
        assert!(!format!("{err}").contains(secret));
        handle.await.expect("server task");
    }
}
