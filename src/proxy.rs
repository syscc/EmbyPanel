use std::{
    collections::HashSet,
    net::IpAddr,
    time::{Duration, Instant},
};

use axum::{
    body::Body,
    http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri, header},
    response::{IntoResponse, Response},
};
use bytes::Bytes;

use crate::{
    auth,
    config::Config,
    diagnostics::{self, DiagnosisStep},
    error::{AppError, AppResult},
    rewrite,
};

const UPSTREAM_RESPONSE_HEADERS_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_PROXY_TEXT_BYTES: usize = 8 * 1024 * 1024;
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
    proxy_to_emby_observed(
        client,
        config,
        method,
        uri,
        headers,
        body,
        peer_ip,
        &mut Default::default(),
    )
    .await
}

pub(crate) async fn proxy_to_emby_observed(
    client: &reqwest::Client,
    config: &Config,
    method: Method,
    uri: &Uri,
    headers: &HeaderMap,
    body: Bytes,
    peer_ip: IpAddr,
    observation: &mut diagnostics::UpstreamObservation,
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

    let response = send_upstream_observed(request, observation).await?;
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
    let mut response =
        proxy_to_reqwest(client, config, method, uri, headers, body, peer_ip).await?;
    let status = rewrite::status_from_reqwest(response.status());
    let headers = copy_response_headers(response.headers(), false);
    if response
        .content_length()
        .is_some_and(|length| length > MAX_PROXY_TEXT_BYTES as u64)
    {
        return Err(AppError::BadGateway(
            "Emby text response exceeds size limit".to_string(),
        ));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(AppError::from)? {
        if bytes.len().saturating_add(chunk.len()) > MAX_PROXY_TEXT_BYTES {
            return Err(AppError::BadGateway(
                "Emby text response exceeds size limit".to_string(),
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    let text = String::from_utf8_lossy(&bytes).into_owned();
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
    send_upstream_observed(request, &mut Default::default()).await
}

async fn send_upstream_observed(
    request: reqwest::RequestBuilder,
    observation: &mut diagnostics::UpstreamObservation,
) -> AppResult<reqwest::Response> {
    let started = Instant::now();
    let response = diagnostics::network::observe_send(observation, async {
        tokio::time::timeout(UPSTREAM_RESPONSE_HEADERS_TIMEOUT, request.send()).await
    })
    .await;
    let response = response
        .map_err(|_| {
            observation.error_kind = Some("timeout".to_string());
            AppError::BadGateway("Emby upstream response headers timed out".to_string())
        })?
        .map_err(|err| {
            observation.error_kind = Some(crate::error::reqwest_error_kind(&err).to_string());
            AppError::from(err)
        })?;
    observation.response_headers_ms = Some(started.elapsed().as_millis() as u64);
    observation.status = Some(response.status().as_u16());
    Ok(response)
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

/// Fixed playback-chain step names in real execution order.
pub(crate) const STEP_EMBY_PATH: &str = "Emby Path";
pub(crate) const STEP_MAPPING: &str = "映射";
pub(crate) const STEP_OPENLIST: &str = "OpenList";
pub(crate) const STEP_RAW_URL: &str = "raw_url";
pub(crate) const STEP_CACHE: &str = "缓存";
pub(crate) const STEP_REDIRECT: &str = "302";
pub(crate) const STEP_INTERNAL_REDIRECT: &str = "内部跳转";
pub(crate) const STEP_PROXY: &str = "Emby Proxy";
pub(crate) const STEP_CONTEXT: &str = "请求上下文";
pub(crate) const STEP_POLICY: &str = "播放策略";

/// A step that is currently waiting on an async operation. Keeping this state in the
/// trace makes cancellation observable even though `Drop` cannot await.
struct ActiveDiagnosisStep {
    name: String,
    started: Instant,
    details: String,
}

/// Builds at most one DiagnosisRecord from real media-resolution branches.
///
/// Production code in `handle_video_stream` starts a step immediately before each
/// fallible/awaited operation and completes it only after the operation returns.
/// If the request future is cancelled, `Drop` emits a sanitized cancellation record
/// synchronously, so no in-flight node disappears from the trace.
pub(crate) struct PlaybackDiagnosisTrace {
    pub(crate) request_id: String,
    pub(crate) context: diagnostics::DiagnosisContext,
    pub(crate) result: diagnostics::DiagnosisResult,
    pub(crate) cache: diagnostics::DiagnosisCache,
    pub(crate) paths: diagnostics::DiagnosisPaths,
    pub(crate) timing: diagnostics::DiagnosisTiming,
    pub(crate) observation: diagnostics::UpstreamObservation,
    operation_intervals: Vec<(Instant, Instant)>,
    cache_duration: Duration,
    failure_log: Option<Box<dyn FnOnce(&diagnostics::DiagnosisRecord) + Send>>,
    server_id: String,
    media_id: String,
    ua: String,
    started: Instant,
    started_at_ms: i64,
    steps: Vec<DiagnosisStep>,
    cache_hit: bool,
    failure_node: Option<String>,
    active_step: Option<ActiveDiagnosisStep>,
    cache_duration_ms: u64,
    cache_started_at_ms: Option<i64>,
    cache_finished_at_ms: Option<i64>,
    cache_pending: bool,
    finished: bool,
}

impl PlaybackDiagnosisTrace {
    pub(crate) fn new(server_id: &str, media_id: &str, ua: &str) -> Self {
        Self {
            request_id: diagnostics::current_request_id()
                .unwrap_or_else(diagnostics::new_request_id),
            context: Default::default(),
            result: Default::default(),
            cache: diagnostics::DiagnosisCache {
                status: Some("not_checked".to_string()),
                ..Default::default()
            },
            paths: Default::default(),
            timing: diagnostics::DiagnosisTiming {
                connection_observation: Some("not_observed".to_string()),
                ..Default::default()
            },
            observation: Default::default(),
            operation_intervals: Vec::new(),
            cache_duration: Duration::ZERO,
            failure_log: None,
            server_id: server_id.to_string(),
            media_id: media_id.to_string(),
            ua: ua.to_string(),
            started: Instant::now(),
            started_at_ms: chrono::Utc::now().timestamp_millis(),
            steps: Vec::with_capacity(6),
            cache_hit: false,
            failure_node: None,
            active_step: None,
            cache_duration_ms: 0,
            cache_started_at_ms: None,
            cache_finished_at_ms: None,
            cache_pending: false,
            finished: false,
        }
    }

    // Anchor all wall-clock timestamps to one clock sample so clock adjustments cannot
    // reverse a running trace. Durations always come from the monotonic clock.
    fn timestamp_at(&self, instant: Instant) -> i64 {
        self.started_at_ms
            .saturating_add(instant.saturating_duration_since(self.started).as_millis() as i64)
    }

    fn timed_step(
        &self,
        name: &str,
        started: Instant,
        finished: Instant,
        details: &str,
    ) -> DiagnosisStep {
        DiagnosisStep {
            name: name.to_string(),
            started_at_ms: Some(self.timestamp_at(started)),
            finished_at_ms: Some(self.timestamp_at(finished)),
            duration_ms: finished.saturating_duration_since(started).as_millis() as u64,
            details: details.to_string(),
            state: Some("recorded".to_string()),
            connection_observation: Some("not_observed".to_string()),
            ..Default::default()
        }
    }

    fn observed_step(&self, mut step: DiagnosisStep) -> DiagnosisStep {
        step.upstream_status = self.observation.status;
        step.response_headers_ms = self.observation.response_headers_ms;
        step.response_body_ms = self.observation.response_body_ms;
        step.error_kind = self.observation.error_kind.clone();
        // Copy before merge_into_timing drains the completed event totals, so
        // step and trace describe the same events without double counting.
        step.name_resolution_ms = self.observation.name_resolution_ms;
        step.name_resolution_calls = self.observation.name_resolution_calls;
        step.connection_setup_ms = self.observation.connection_setup_ms;
        step.connection_attempts = self.observation.connection_attempts;
        if self.observation.connection_observation.is_some() {
            step.connection_observation = self.observation.connection_observation.clone();
        }
        step
    }

    /// Start a real operation. If the future is cancelled before completion, this
    /// is the node that `Drop` marks as cancelled.
    pub(crate) fn begin_step(&mut self, name: &str, details: &str) {
        if self.finished || self.failure_node.is_some() {
            return;
        }
        self.observation = Default::default();
        self.active_step = Some(ActiveDiagnosisStep {
            name: name.to_string(),
            started: Instant::now(),
            details: details.to_string(),
        });
    }

    /// Complete the currently active operation. Cache operations are accumulated
    /// into one logical cache node so the public order stays Emby → mapping →
    /// OpenList → raw_url → cache → 302 even when cache lookup happens first.
    pub(crate) fn complete_step(&mut self, details: &str) {
        let Some(active) = self.active_step.take() else {
            return;
        };
        let finished = Instant::now();
        self.operation_intervals.push((active.started, finished));
        let step =
            self.observed_step(self.timed_step(&active.name, active.started, finished, details));
        diagnostics::network::merge_into_timing(&mut self.observation, &mut self.timing);
        if active.name == STEP_CACHE {
            self.cache_duration += finished.saturating_duration_since(active.started);
            self.cache_duration_ms = self.cache_duration.as_millis() as u64;
            self.cache_started_at_ms = self.cache_started_at_ms.or(step.started_at_ms);
            self.cache_finished_at_ms = step.finished_at_ms;
            self.cache_pending = true;
        } else {
            self.steps.push(step);
        }
    }

    /// Record a non-failure step with monotonic elapsed time from `started`.
    /// No-ops after the first failure or after finish (prevents forged later success).
    pub(crate) fn push_step(&mut self, name: &str, started: Instant, details: &str) {
        if self.finished || self.failure_node.is_some() {
            return;
        }
        let finished = Instant::now();
        self.operation_intervals.push((started, finished));
        self.steps
            .push(self.timed_step(name, started, finished, details));
    }

    /// Bypasses are decisions, not real busy intervals.
    pub(crate) fn bypass_steps(&mut self, names: &[&str]) {
        for name in names {
            self.skip_step(name, "bypassed");
        }
    }

    pub(crate) fn skip_step(&mut self, name: &str, reason: &str) {
        if self.finished || self.failure_node.is_some() {
            return;
        }
        let now = Instant::now();
        let mut step = self.timed_step(name, now, now, "bypassed");
        step.state = Some("skipped".to_string());
        step.reason = Some(reason.to_string());
        self.steps.push(step);
    }

    /// Append the deferred logical cache node after raw URL resolution.
    pub(crate) fn append_cache_step(&mut self, details: &str) {
        if self.finished || self.failure_node.is_some() || !self.cache_pending {
            return;
        }
        self.steps.push(DiagnosisStep {
            name: STEP_CACHE.to_string(),
            started_at_ms: self.cache_started_at_ms,
            finished_at_ms: self.cache_finished_at_ms,
            duration_ms: self.cache_duration_ms,
            details: details.to_string(),
            state: Some("recorded".to_string()),
            connection_observation: Some("not_observed".to_string()),
            ..Default::default()
        });
        self.cache_pending = false;
    }

    /// Record the first failure node. Subsequent push/fail calls are ignored.
    pub(crate) fn fail_step(&mut self, name: &str, started: Instant, details: &str) {
        if self.finished || self.failure_node.is_some() {
            return;
        }
        let active_started = self
            .active_step
            .take()
            .filter(|active| active.name == name)
            .map(|active| active.started);
        let started = active_started.unwrap_or(started);
        let finished = Instant::now();
        self.operation_intervals.push((started, finished));
        let mut step = self.observed_step(self.timed_step(name, started, finished, details));
        diagnostics::network::merge_into_timing(&mut self.observation, &mut self.timing);
        let cancelled = details.starts_with("cancelled");
        step.state = Some(if cancelled { "cancelled" } else { "failed" }.to_string());
        step.reason = Some(details.to_string());
        self.result.outcome = Some(if cancelled { "cancelled" } else { "failed" }.to_string());
        if cancelled {
            self.result.status_code = None;
        }
        self.result.reason = Some(details.to_string());
        self.result.error_kind = step.error_kind.clone();
        self.result.upstream_status = step.upstream_status;
        if name == STEP_CACHE {
            self.cache_pending = false;
            self.cache_duration += finished.saturating_duration_since(started);
            self.cache_duration_ms = self.cache_duration.as_millis() as u64;
            step.duration_ms = self.cache_duration_ms;
            step.started_at_ms = self.cache_started_at_ms.or(step.started_at_ms);
        }
        self.steps.push(step);
        self.failure_node = Some(name.to_string());
    }

    /// One-shot fallback for existing request/activity logs when no normal log path completed.
    pub(crate) fn on_unlogged_failure(
        &mut self,
        callback: impl FnOnce(&diagnostics::DiagnosisRecord) + Send + 'static,
    ) {
        self.failure_log = Some(Box::new(callback));
    }

    pub(crate) fn failure_logged(&mut self) {
        self.failure_log = None;
    }

    pub(crate) fn mark_upstream_status_failure(&mut self) {
        if let Some(step) = self.steps.last_mut() {
            step.state = Some("failed".to_string());
            step.error_kind = Some("upstream_http".to_string());
            step.reason = Some("upstream returned non-success status".to_string());
            self.failure_node = Some(step.name.clone());
        }
    }

    pub(crate) fn fail_error(&mut self, name: &str, error: &AppError) {
        self.observation
            .error_kind
            .get_or_insert_with(|| error.diagnostic_kind().to_string());
        self.fail_step(name, Instant::now(), &error.safe_log_message());
        self.result.status_code = Some(error.status().as_u16());
    }

    pub(crate) fn set_redirect(&mut self, url: &str, reason: &str) {
        self.result.outcome = Some("redirect".to_string());
        self.result.status_code = Some(302);
        self.result.target_host = diagnostics::target_host(url);
        self.result.reason = Some(reason.to_string());
        self.paths.final_url = Some(diagnostics::sanitize_path(url));
    }

    /// Internal redirect failure is a real failed step but falls back to the original URL.
    pub(crate) fn complete_redirect_fallback(&mut self, error: &AppError) {
        self.observation
            .error_kind
            .get_or_insert_with(|| error.diagnostic_kind().to_string());
        self.complete_step(&error.safe_log_message());
        if let Some(step) = self.steps.last_mut() {
            step.state = Some("failed".to_string());
            step.reason = Some("using original URL after internal redirect failure".to_string());
        }
    }

    pub(crate) fn set_cache_hit(&mut self, hit: bool) {
        if !self.finished && self.failure_node.is_none() {
            self.cache_hit = hit;
        }
    }

    #[cfg(test)]
    pub(crate) fn step_names(&self) -> Vec<&str> {
        self.steps.iter().map(|s| s.name.as_str()).collect()
    }

    #[cfg(test)]
    pub(crate) fn failure_node(&self) -> Option<&str> {
        self.failure_node.as_deref()
    }

    #[cfg(test)]
    pub(crate) fn cache_hit(&self) -> bool {
        self.cache_hit
    }

    #[cfg(test)]
    pub(crate) fn steps(&self) -> &[DiagnosisStep] {
        &self.steps
    }

    /// Emit one record via the shared collector. Safe to call once; extra calls no-op.
    pub(crate) async fn finish(mut self) {
        self.record();
    }

    fn record(&mut self) {
        if self.finished {
            return;
        }
        self.finished = true;
        if self.server_id.trim().is_empty() || self.media_id.trim().is_empty() {
            return;
        }
        let finished = Instant::now();
        // Lookup/set work must remain observable even when a later node fails.
        if self.cache_pending {
            self.steps.push(DiagnosisStep {
                name: STEP_CACHE.to_string(),
                started_at_ms: self.cache_started_at_ms,
                finished_at_ms: self.cache_finished_at_ms,
                duration_ms: self.cache_duration_ms,
                details: self
                    .cache
                    .status
                    .clone()
                    .unwrap_or_else(|| "not_checked".to_string()),
                state: Some("recorded".to_string()),
                reason: self.cache.reason.clone(),
                connection_observation: Some("not_observed".to_string()),
                ..Default::default()
            });
            self.cache_pending = false;
        }
        let mut record = diagnostics::create_diagnosis_record(
            &self.server_id,
            &self.media_id,
            &self.ua,
            std::mem::take(&mut self.steps),
            self.cache_hit,
            self.failure_node.take(),
        );
        record.request_id = self.request_id.clone();
        record.context = std::mem::take(&mut self.context);
        record.result = std::mem::take(&mut self.result);
        record.cache = std::mem::take(&mut self.cache);
        record.paths = std::mem::take(&mut self.paths);
        diagnostics::network::merge_into_timing(&mut self.observation, &mut self.timing);
        record.timing = std::mem::take(&mut self.timing);
        if let Some(step) = record
            .steps
            .iter()
            .filter(|step| step.state.as_deref() != Some("skipped"))
            .max_by_key(|step| step.duration_ms)
        {
            record.timing.slowest_step = Some(step.name.clone());
            record.timing.slowest_step_ms = Some(step.duration_ms);
        }
        record.timing.unaccounted_ms = Some(
            unaccounted_duration(self.started, finished, &mut self.operation_intervals).as_millis()
                as u64,
        );
        record.started_at_ms = Some(self.started_at_ms);
        record.finished_at_ms = Some(self.timestamp_at(finished));
        record.duration_ms = Some(finished.duration_since(self.started).as_millis() as u64);
        let record = diagnostics::sanitize_diagnosis_record(record);
        if matches!(
            record.result.outcome.as_deref(),
            Some("failed" | "cancelled")
        ) {
            if let Some(callback) = self.failure_log.take() {
                callback(&record);
            }
        }
        tracing::info!(request_id = %record.request_id, server_id = %record.server_id,
            media_id = %record.media_id, outcome = ?record.result.outcome,
            status_code = ?record.result.status_code, "playback diagnosis completed");
        diagnostics::record_diagnosis_now(record);
    }
}

impl Drop for PlaybackDiagnosisTrace {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        if let Some(active) = self.active_step.take() {
            self.fail_step(
                &active.name,
                active.started,
                &format!("cancelled: {}", active.details),
            );
        } else if self.failure_node.is_none() {
            // A trace created immediately before an unexpected cancellation still
            // produces a useful, sanitized record instead of disappearing.
            let now = Instant::now();
            self.steps.push(self.timed_step(
                STEP_EMBY_PATH,
                now,
                now,
                "cancelled before step started",
            ));
            if let Some(step) = self.steps.last_mut() {
                step.state = Some("cancelled".to_string());
            }
            self.failure_node = Some(STEP_EMBY_PATH.to_string());
            self.result.outcome = Some("cancelled".to_string());
            self.result.reason = Some("cancelled before step started".to_string());
        }
        self.record();
    }
}

/// Union monotonic operation intervals before rounding; cache spans and bypasses are excluded.
fn unaccounted_duration(
    start: Instant,
    end: Instant,
    intervals: &mut [(Instant, Instant)],
) -> Duration {
    intervals.sort_by_key(|interval| interval.0);
    let mut covered = Duration::ZERO;
    let mut cursor = start;
    for &(from, until) in intervals.iter() {
        let from = from.max(start).max(cursor).min(end);
        let until = until.min(end).max(from);
        covered += until.saturating_duration_since(from);
        cursor = cursor.max(until);
    }
    end.saturating_duration_since(start).saturating_sub(covered)
}

#[cfg(test)]
mod tests {
    use axum::body::HttpBody;
    use reqwest::header::{HeaderMap as ReqwestHeaderMap, HeaderValue as ReqwestHeaderValue};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
        task::JoinHandle,
    };

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
                block_web_ui_status_code: 403,
                block_web_ui_message: "Emby Web UI is disabled by EmbyPanel".to_string(),
                real_ip_mode: real_ip_mode.to_string(),
                real_ip_header: real_ip_header.to_string(),
                trusted_proxy_cidrs: "192.0.2.0/24".to_string(),
                trusted_proxy_networks: vec!["192.0.2.0/24".parse().unwrap()],
                ..EmbyServerConfig::default()
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

    async fn mock_upstream<F, Fut>(respond: F) -> (Config, reqwest::Client, JoinHandle<()>)
    where
        F: FnOnce(TcpStream) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut config = test_config("auto", "");
        config.emby_host = format!("http://{}", listener.local_addr().unwrap());
        config.servers[0].emby_host = config.emby_host.clone();
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        let task = tokio::spawn(async move {
            tokio::time::timeout(Duration::from_secs(5), async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    let byte = socket.read_u8().await.unwrap();
                    request.push(byte);
                    assert!(request.len() <= 8192);
                }
                respond(socket).await;
            })
            .await
            .expect("local upstream timed out");
        });
        (config, client, task)
    }

    async fn request_mock_text(
        config: &Config,
        client: &reqwest::Client,
    ) -> AppResult<(StatusCode, HeaderMap, String)> {
        proxy_text(
            client,
            config,
            Method::GET,
            &"/Items?api_key=mock-secret".parse().unwrap(),
            &HeaderMap::new(),
            Bytes::new(),
            "127.0.0.1".parse().unwrap(),
        )
        .await
    }

    #[tokio::test]
    async fn proxy_text_reads_normal_text_and_preserves_status_and_headers() {
        let expected = "{\"Name\":\"中文\"}";
        let (config, client, task) = mock_upstream(move |mut socket| async move {
            let response = format!(
                "HTTP/1.1 201 Created\r\nContent-Length: {}\r\nContent-Type: application/json\r\nX-Mock: yes\r\nConnection: close\r\n\r\n{expected}",
                expected.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        })
        .await;

        let (status, headers, text) = request_mock_text(&config, &client).await.unwrap();
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(text, expected);
        assert_eq!(headers[header::CONTENT_TYPE], "application/json");
        assert_eq!(headers["x-mock"], "yes");
        assert!(!headers.contains_key(header::CONTENT_LENGTH));
        assert!(!headers.contains_key(header::CONNECTION));
        task.await.unwrap();
    }

    #[tokio::test]
    async fn proxy_text_preserves_lossy_utf8_and_multibyte_chunk_boundaries() {
        let (config, client, task) = mock_upstream(|mut socket| async move {
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n1\r\n\xe4\r\n2\r\n\xb8\xad\r\n1\r\n\xff\r\n0\r\n\r\n")
                .await
                .unwrap();
        })
        .await;

        let (_, _, text) = request_mock_text(&config, &client).await.unwrap();
        assert_eq!(text, "中\u{fffd}");
        task.await.unwrap();
    }

    #[tokio::test]
    async fn proxy_text_rejects_declared_length_above_limit_without_reading_body() {
        let (config, client, task) = mock_upstream(|mut socket| async move {
            let headers = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
                MAX_PROXY_TEXT_BYTES + 1
            );
            socket.write_all(headers.as_bytes()).await.unwrap();
            // No body is sent: rejection must occur from the headers alone.
            assert_eq!(socket.read(&mut [0; 1]).await.unwrap(), 0);
        })
        .await;

        let error = request_mock_text(&config, &client).await.unwrap_err();
        assert!(matches!(error, AppError::BadGateway(_)));
        assert_eq!(
            error.safe_log_message(),
            "Emby text response exceeds size limit"
        );
        assert_eq!(error.into_response().status(), StatusCode::BAD_GATEWAY);
        task.await.unwrap();
    }

    #[tokio::test]
    async fn proxy_text_accepts_exact_limit() {
        let (config, client, task) = mock_upstream(|mut socket| async move {
            let headers = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
                MAX_PROXY_TEXT_BYTES
            );
            socket.write_all(headers.as_bytes()).await.unwrap();
            let chunk = vec![b'x'; 64 * 1024];
            for _ in 0..MAX_PROXY_TEXT_BYTES / chunk.len() {
                socket.write_all(&chunk).await.unwrap();
            }
        })
        .await;

        let (_, _, text) = request_mock_text(&config, &client).await.unwrap();
        assert_eq!(text.len(), MAX_PROXY_TEXT_BYTES);
        assert!(text.bytes().all(|byte| byte == b'x'));
        task.await.unwrap();
    }

    #[tokio::test]
    async fn proxy_text_rejects_chunked_body_above_limit_before_end_of_stream() {
        let (config, client, task) = mock_upstream(|mut socket| async move {
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n")
                .await
                .unwrap();
            let chunk = vec![b'x'; 64 * 1024];
            for _ in 0..MAX_PROXY_TEXT_BYTES / chunk.len() {
                socket.write_all(b"10000\r\n").await.unwrap();
                socket.write_all(&chunk).await.unwrap();
                socket.write_all(b"\r\n").await.unwrap();
            }
            socket.write_all(b"1\r\nx\r\n").await.unwrap();
            // Leave the stream unfinished to verify prompt rejection and cancellation.
            assert_eq!(socket.read(&mut [0; 1]).await.unwrap(), 0);
        })
        .await;

        let error = request_mock_text(&config, &client).await.unwrap_err();
        assert!(matches!(error, AppError::BadGateway(_)));
        assert_eq!(
            error.safe_log_message(),
            "Emby text response exceeds size limit"
        );
        task.await.unwrap();
    }

    #[tokio::test]
    async fn proxy_text_body_read_error_uses_safe_messages() {
        let (config, client, task) = mock_upstream(|mut socket| async move {
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nshort")
                .await
                .unwrap();
        })
        .await;

        let error = request_mock_text(&config, &client).await.unwrap_err();
        assert!(matches!(error, AppError::Http(_)));
        assert!(!error.safe_log_message().contains("mock-secret"));
        let response = error.into_response();
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        let body = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap();
        assert_eq!(body.as_ref(), b"bad gateway");
        task.await.unwrap();
    }

    #[tokio::test]
    async fn media_response_streams_first_chunk_and_cancels_upstream_on_drop() {
        let (config, client, task) = mock_upstream(|mut socket| async move {
            socket
                .write_all(b"HTTP/1.1 206 Partial Content\r\nTransfer-Encoding: chunked\r\nContent-Type: video/mp4\r\nContent-Range: bytes 0-9/10\r\n\r\n5\r\nfirst\r\n")
                .await
                .unwrap();
            assert_eq!(socket.read(&mut [0; 1]).await.unwrap(), 0);
        })
        .await;

        let response = proxy_to_emby(
            &client,
            &config,
            Method::GET,
            &"/Videos/mock/stream".parse().unwrap(),
            &HeaderMap::new(),
            Bytes::new(),
            "127.0.0.1".parse().unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(response.headers()[header::CONTENT_RANGE], "bytes 0-9/10");
        let mut body = Box::pin(response.into_body());
        let frame = tokio::time::timeout(
            Duration::from_secs(2),
            std::future::poll_fn(|cx| body.as_mut().poll_frame(cx)),
        )
        .await
        .expect("first media chunk should arrive before the body completes")
        .unwrap()
        .unwrap();
        assert_eq!(frame.into_data().unwrap().as_ref(), b"first");
        drop(body);
        task.await.unwrap();
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

    fn assert_diagnosis_timing(record: &diagnostics::DiagnosisRecord) {
        let started = record.started_at_ms.expect("trace start timestamp");
        let finished = record.finished_at_ms.expect("trace end timestamp");
        assert!(started > 0);
        assert_eq!(finished - started, record.duration_ms.unwrap() as i64);
        for step in &record.steps {
            let step_start = step.started_at_ms.expect("step start timestamp");
            let step_end = step.finished_at_ms.expect("step end timestamp");
            assert!(step_start >= started);
            assert!(step_end >= step_start);
            assert!(step_end <= finished);
        }
    }

    #[test]
    fn operation_union_excludes_cache_gaps_overlaps_and_rounding_artifacts() {
        let start = Instant::now();
        let at = |micros| start + Duration::from_micros(micros);
        let mut intervals = [(at(5000), at(5600)), (at(0), at(600)), (at(200), at(900))];
        assert_eq!(
            unaccounted_duration(start, at(10000), &mut intervals),
            Duration::from_micros(8500)
        );
        let mut rounded = [(at(0), at(600)), (at(600), at(1200))];
        assert_eq!(
            unaccounted_duration(start, at(1200), &mut rounded),
            Duration::ZERO
        );
    }

    #[tokio::test]
    async fn playback_trace_failure_callback_is_sanitized_once_and_can_be_disarmed() {
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut trace = PlaybackDiagnosisTrace::new("s", "callback", "UA");
        let shared = calls.clone();
        trace.on_unlogged_failure(move |record| {
            assert_eq!(record.result.outcome.as_deref(), Some("cancelled"));
            assert!(!record.steps[0].details.contains("synthetic-secret"));
            shared.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        });
        trace.begin_step(STEP_EMBY_PATH, "query token=synthetic-secret");
        drop(trace);
        assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 1);
        let mut trace = PlaybackDiagnosisTrace::new("s", "already-logged", "UA");
        trace.on_unlogged_failure(move |_| panic!("disarmed callback must not run"));
        trace.fail_error(
            STEP_EMBY_PATH,
            &AppError::BadGateway("upstream failed".to_string()),
        );
        trace.failure_logged();
        trace.finish().await;
    }

    #[tokio::test]
    async fn playback_trace_network_totals_merge_on_success_failure_and_cancel_without_double_counting()
     {
        for outcome in ["success", "failure", "cancel"] {
            diagnostics::clean_diagnostics();
            let mut trace = PlaybackDiagnosisTrace::new("s", "network-totals", "UA");
            trace.begin_step(STEP_EMBY_PATH, "query");
            trace.observation.name_resolution_ms = Some(700);
            trace.observation.name_resolution_calls = Some(1);
            trace.observation.connection_setup_ms = Some(900);
            trace.observation.connection_attempts = Some(1);
            trace.observation.connection_observation = Some("observed".to_string());
            trace.complete_step("query completed");
            trace.begin_step(STEP_OPENLIST, "get");
            trace.observation.name_resolution_ms = Some(800);
            trace.observation.name_resolution_calls = Some(2);
            trace.observation.connection_setup_ms = Some(1000);
            trace.observation.connection_attempts = Some(2);
            trace.observation.connection_observation = Some(
                if outcome == "cancel" {
                    "partial"
                } else {
                    "observed"
                }
                .to_string(),
            );
            match outcome {
                "success" => {
                    trace.complete_step("completed");
                    trace.finish().await;
                }
                "failure" => {
                    trace.fail_error(
                        STEP_OPENLIST,
                        &AppError::BadGateway("synthetic failure".to_string()),
                    );
                    trace.finish().await;
                }
                _ => drop(trace),
            }
            let record = diagnostics::get_diagnosis_records().pop().unwrap();
            assert_eq!(record.timing.name_resolution_ms, Some(1500));
            assert_eq!(record.timing.name_resolution_calls, Some(3));
            assert_eq!(record.timing.connection_setup_ms, Some(1900));
            assert_eq!(record.timing.connection_attempts, Some(3));
            assert_eq!(
                record.timing.connection_observation.as_deref(),
                Some(if outcome == "cancel" {
                    "partial"
                } else {
                    "observed"
                })
            );
            assert!(record.timing.unaccounted_ms.unwrap() <= record.duration_ms.unwrap());
            assert_eq!(record.steps[0].name_resolution_ms, Some(700));
            assert_eq!(record.steps[0].name_resolution_calls, Some(1));
            assert_eq!(record.steps[0].connection_setup_ms, Some(900));
            assert_eq!(record.steps[0].connection_attempts, Some(1));
            assert_eq!(
                record.steps[0].connection_observation.as_deref(),
                Some("observed")
            );
            assert_eq!(record.steps[1].name_resolution_ms, Some(800));
            assert_eq!(record.steps[1].name_resolution_calls, Some(2));
            assert_eq!(record.steps[1].connection_setup_ms, Some(1000));
            assert_eq!(record.steps[1].connection_attempts, Some(2));
            assert_eq!(
                record.steps[1].connection_observation,
                record.timing.connection_observation
            );
        }
    }

    #[tokio::test]
    async fn playback_trace_timing_covers_total_and_deferred_cache_operations() {
        diagnostics::clean_diagnostics();
        let mut trace = PlaybackDiagnosisTrace::new("server-1", "timed-success", "UA");
        let started_at_ms = trace.started_at_ms;
        trace.begin_step(STEP_CACHE, "lookup");
        trace.complete_step("miss");
        let cache_started_at_ms = trace.cache_started_at_ms;
        let first_cache_duration = trace.cache_duration_ms;

        // The cache node spans lookup and set, but its duration excludes work in between.
        tokio::time::sleep(std::time::Duration::from_millis(8)).await;
        trace.begin_step(STEP_EMBY_PATH, "query");
        trace.complete_step("resolved");
        trace.begin_step(STEP_CACHE, "set");
        trace.complete_step("miss");
        let cache_finished_at_ms = trace.cache_finished_at_ms;
        trace.bypass_steps(&[STEP_MAPPING, STEP_OPENLIST, STEP_RAW_URL]);
        trace.append_cache_step("miss");
        trace.push_step(STEP_REDIRECT, Instant::now(), "redirect");
        trace.finish().await;

        let records = diagnostics::get_diagnosis_records();
        assert_eq!(records.len(), 1);
        let record = &records[0];
        assert_diagnosis_timing(record);
        assert_eq!(record.started_at_ms, Some(started_at_ms));
        let cache = record
            .steps
            .iter()
            .find(|step| step.name == STEP_CACHE)
            .unwrap();
        assert_eq!(cache.started_at_ms, cache_started_at_ms);
        assert_eq!(cache.finished_at_ms, cache_finished_at_ms);
        assert!(cache.duration_ms >= first_cache_duration);
        let cache_span = cache.finished_at_ms.unwrap() - cache.started_at_ms.unwrap();
        assert!(cache_span >= cache.duration_ms as i64 + 5);
        assert!(record.duration_ms.unwrap() > record.steps.iter().map(|s| s.duration_ms).sum());
    }

    #[tokio::test]
    async fn playback_trace_failure_and_cache_cancellation_retain_original_start() {
        for cancelled in [false, true] {
            diagnostics::clean_diagnostics();
            let mut trace = PlaybackDiagnosisTrace::new("server-1", "timed-failure", "UA");
            trace.begin_step(STEP_CACHE, "lookup");
            trace.complete_step("miss");
            let cache_started_at_ms = trace.cache_started_at_ms;
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
            trace.begin_step(STEP_CACHE, "set");
            if cancelled {
                drop(trace);
            } else {
                trace.fail_step(STEP_CACHE, Instant::now(), "failed");
                trace.finish().await;
            }
            let records = diagnostics::get_diagnosis_records();
            assert_eq!(records.len(), 1);
            let record = &records[0];
            assert_diagnosis_timing(record);
            assert_eq!(record.failure_node.as_deref(), Some(STEP_CACHE));
            assert_eq!(record.steps[0].started_at_ms, cache_started_at_ms);
            assert_eq!(record.steps[0].details.starts_with("cancelled:"), cancelled);
        }
    }

    #[tokio::test]
    async fn playback_trace_active_failure_uses_operation_start_and_empty_drop_has_timing() {
        diagnostics::clean_diagnostics();
        let mut trace = PlaybackDiagnosisTrace::new("server-1", "timed-active", "UA");
        trace.begin_step(STEP_OPENLIST, "fs_get");
        let started_at_ms = trace.timestamp_at(trace.active_step.as_ref().unwrap().started);
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        trace.fail_step(STEP_OPENLIST, Instant::now(), "failed");
        trace.finish().await;
        drop(PlaybackDiagnosisTrace::new("server-1", "timed-empty", "UA"));
        let records = diagnostics::get_diagnosis_records();
        assert_eq!(records.len(), 2);
        for record in &records {
            assert_diagnosis_timing(record);
        }
        assert_eq!(records[0].steps[0].started_at_ms, Some(started_at_ms));
        assert!(records[0].steps[0].duration_ms >= 2);
        assert_eq!(records[1].steps[0].details, "cancelled before step started");
    }
    #[tokio::test]
    async fn playback_trace_success_chain_order_cache_hit_and_redirect() {
        diagnostics::init_diagnostics();
        diagnostics::clean_diagnostics();

        let mut trace = PlaybackDiagnosisTrace::new("server-1", "item-42", "TestPlayer/1.0");
        let t0 = Instant::now();
        // Cache short-circuit: earlier nodes bypassed, cache hit, 302 redirect.
        trace.bypass_steps(&[STEP_EMBY_PATH, STEP_MAPPING, STEP_OPENLIST, STEP_RAW_URL]);
        trace.push_step(STEP_CACHE, t0, "hit");
        trace.set_cache_hit(true);
        trace.push_step(STEP_REDIRECT, Instant::now(), "redirect");
        assert!(trace.cache_hit());
        assert!(trace.failure_node().is_none());
        assert_eq!(
            trace.step_names(),
            vec![
                STEP_EMBY_PATH,
                STEP_MAPPING,
                STEP_OPENLIST,
                STEP_RAW_URL,
                STEP_CACHE,
                STEP_REDIRECT,
            ]
        );
        for step in trace.steps() {
            assert!(
                step.duration_ms < 10_000,
                "duration unbounded: {}",
                step.duration_ms
            );
        }
        trace.finish().await;
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        let records = diagnostics::get_diagnosis_records();
        let r = records
            .iter()
            .find(|r| r.media_id == "item-42")
            .expect("expected diagnosis for item-42");
        assert_eq!(r.server_id, "server-1");
        assert!(r.cache_hit);
        assert!(r.failure_node.is_none());
        assert_eq!(r.steps.len(), 6);
        assert_eq!(r.steps[4].details, "hit");
        assert_eq!(r.steps[5].details, "redirect");
        // Single finish → single record for this media id.
        assert_eq!(
            records.iter().filter(|r| r.media_id == "item-42").count(),
            1
        );
    }

    #[tokio::test]
    async fn playback_trace_first_failure_blocks_later_success_and_sets_node() {
        diagnostics::init_diagnostics();
        diagnostics::clean_diagnostics();

        let mut trace = PlaybackDiagnosisTrace::new("server-1", "item-map-fail", "UA");
        trace.push_step(STEP_EMBY_PATH, Instant::now(), "matched");
        trace.fail_step(STEP_MAPPING, Instant::now(), "failed");
        // Must not forge subsequent success after first failure.
        trace.push_step(STEP_OPENLIST, Instant::now(), "matched");
        trace.push_step(STEP_RAW_URL, Instant::now(), "matched");
        trace.push_step(STEP_CACHE, Instant::now(), "hit");
        trace.set_cache_hit(true);
        trace.push_step(STEP_REDIRECT, Instant::now(), "redirect");
        assert_eq!(trace.failure_node(), Some(STEP_MAPPING));
        assert_eq!(trace.step_names(), vec![STEP_EMBY_PATH, STEP_MAPPING]);
        assert!(!trace.cache_hit());

        let mut trace2 = PlaybackDiagnosisTrace::new("server-1", "item-ol-fail", "UA");
        trace2.push_step(STEP_EMBY_PATH, Instant::now(), "matched");
        trace2.push_step(STEP_MAPPING, Instant::now(), "bypassed");
        trace2.fail_step(STEP_OPENLIST, Instant::now(), "failed");
        trace2.push_step(STEP_RAW_URL, Instant::now(), "matched");
        assert_eq!(trace2.failure_node(), Some(STEP_OPENLIST));
        assert_eq!(
            trace2.step_names(),
            vec![STEP_EMBY_PATH, STEP_MAPPING, STEP_OPENLIST]
        );
        trace.finish().await;
        trace2.finish().await;
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        let records = diagnostics::get_diagnosis_records();
        let r_map = records
            .iter()
            .find(|r| r.media_id == "item-map-fail")
            .expect("mapping fail record");
        assert_eq!(r_map.failure_node.as_deref(), Some(STEP_MAPPING));
        assert_eq!(r_map.steps.len(), 2);
        assert!(!r_map.cache_hit);
        let r = records
            .iter()
            .find(|r| r.media_id == "item-ol-fail")
            .expect("openlist fail record");
        assert_eq!(r.failure_node.as_deref(), Some(STEP_OPENLIST));
        assert_eq!(r.steps.len(), 3);
        assert!(
            !r.steps
                .iter()
                .any(|s| s.name == STEP_RAW_URL || s.name == STEP_REDIRECT)
        );
    }

    #[tokio::test]
    async fn playback_trace_cache_miss_and_proxy_302_details() {
        diagnostics::init_diagnostics();
        diagnostics::clean_diagnostics();

        let mut hit = PlaybackDiagnosisTrace::new("server-1", "item-redirect", "UA");
        hit.push_step(STEP_EMBY_PATH, Instant::now(), "matched");
        hit.push_step(STEP_MAPPING, Instant::now(), "matched");
        hit.bypass_steps(&[STEP_OPENLIST, STEP_RAW_URL]);
        hit.push_step(STEP_CACHE, Instant::now(), "miss");
        hit.set_cache_hit(false);
        hit.push_step(STEP_REDIRECT, Instant::now(), "redirect");
        hit.finish().await;

        let mut proxy = PlaybackDiagnosisTrace::new("server-1", "item-proxy", "UA");
        proxy.push_step(STEP_EMBY_PATH, Instant::now(), "bypassed");
        proxy.bypass_steps(&[STEP_MAPPING, STEP_OPENLIST, STEP_RAW_URL, STEP_CACHE]);
        proxy.push_step(STEP_REDIRECT, Instant::now(), "proxy");
        proxy.finish().await;
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        let records = diagnostics::get_diagnosis_records();
        let r_ok = records
            .iter()
            .find(|r| r.media_id == "item-redirect")
            .expect("redirect");
        let r_proxy = records
            .iter()
            .find(|r| r.media_id == "item-proxy")
            .expect("proxy");
        assert!(!r_ok.cache_hit);
        assert_eq!(
            r_ok.steps
                .iter()
                .find(|s| s.name == STEP_CACHE)
                .unwrap()
                .details,
            "miss"
        );
        assert_eq!(r_ok.steps.last().unwrap().details, "redirect");
        assert_eq!(r_proxy.steps.last().unwrap().details, "proxy");
        assert!(r_proxy.failure_node.is_none());
    }

    #[tokio::test]
    async fn playback_trace_skips_non_eligible_without_ids() {
        diagnostics::init_diagnostics();
        diagnostics::clean_diagnostics();

        PlaybackDiagnosisTrace::new("", "item-1", "UA")
            .finish()
            .await;
        PlaybackDiagnosisTrace::new("server-1", "", "UA")
            .finish()
            .await;
        // Even with a step, empty identity must stay silent.
        let mut t = PlaybackDiagnosisTrace::new("", "x", "UA");
        t.push_step(STEP_EMBY_PATH, Instant::now(), "matched");
        t.finish().await;
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let records = diagnostics::get_diagnosis_records();
        assert!(
            !records
                .iter()
                .any(|r| r.media_id.is_empty() || r.server_id.is_empty())
        );
    }

    #[tokio::test]
    async fn playback_trace_details_are_source_minimized_and_sanitized() {
        diagnostics::init_diagnostics();
        diagnostics::clean_diagnostics();

        let mut trace =
            PlaybackDiagnosisTrace::new("server-1", "item-leak", "UA with token=should-not-leak");
        // Production must only push safe details; this guards sanitizer if a bad detail slips in.
        trace.push_step(STEP_EMBY_PATH, Instant::now(), "matched");
        trace.push_step(
            STEP_RAW_URL,
            Instant::now(),
            "https://user:secret@host/path?token=abc123&api_key=xyz",
        );
        trace.fail_step(
            STEP_REDIRECT,
            Instant::now(),
            "Authorization: Bearer secret-token; Cookie: a=1",
        );
        // After fail, later success must not appear.
        trace.push_step(STEP_CACHE, Instant::now(), "hit");
        trace.finish().await;
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        let records = diagnostics::get_diagnosis_records();
        let r = records
            .iter()
            .find(|r| r.media_id == "item-leak")
            .expect("item-leak");
        assert_eq!(r.failure_node.as_deref(), Some(STEP_REDIRECT));
        assert_eq!(r.steps.len(), 3);
        let joined = format!(
            "{}|{}|{}",
            r.ua,
            r.steps
                .iter()
                .map(|s| s.details.as_str())
                .collect::<Vec<_>>()
                .join("|"),
            r.sanitized_errors.join("|")
        );
        assert!(!joined.contains("secret"));
        assert!(!joined.contains("abc123"));
        assert!(!joined.contains("xyz"));
        assert!(!joined.contains("Bearer secret-token"));
        assert!(
            joined.contains(diagnostics::REDACTED_PLACEHOLDER) || !r.sanitized_errors.is_empty()
        );
    }

    #[tokio::test]
    async fn playback_trace_finish_is_single_shot() {
        diagnostics::init_diagnostics();
        diagnostics::clean_diagnostics();

        let mut trace = PlaybackDiagnosisTrace::new("server-1", "item-once", "UA");
        trace.push_step(STEP_EMBY_PATH, Instant::now(), "matched");
        trace.push_step(STEP_REDIRECT, Instant::now(), "redirect");
        // finish consumes self — second record only if we build another trace.
        trace.finish().await;
        let mut again = PlaybackDiagnosisTrace::new("server-1", "item-once", "UA");
        again.push_step(STEP_EMBY_PATH, Instant::now(), "matched");
        again.finish().await;
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let count = diagnostics::get_diagnosis_records()
            .iter()
            .filter(|r| r.media_id == "item-once")
            .count();
        // Two separate traces = two records; documents that callers must create one trace per request.
        assert_eq!(count, 2);
    }

    #[tokio::test]
    async fn playback_trace_cache_hit_bypasses_front_steps_and_sets_hit() {
        diagnostics::init_diagnostics();
        diagnostics::clean_diagnostics();

        let mut trace = PlaybackDiagnosisTrace::new("server-1", "item-cache-hit", "UA");
        trace.bypass_steps(&[STEP_EMBY_PATH, STEP_MAPPING, STEP_OPENLIST, STEP_RAW_URL]);
        trace.push_step(STEP_CACHE, Instant::now(), "hit");
        trace.set_cache_hit(true);
        trace.push_step(STEP_REDIRECT, Instant::now(), "redirect");
        trace.finish().await;

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let records = diagnostics::get_diagnosis_records();
        let r = records
            .iter()
            .find(|r| r.media_id == "item-cache-hit")
            .expect("record");
        assert_eq!(
            r.steps.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            vec!["Emby Path", "映射", "OpenList", "raw_url", "缓存", "302"]
        );
        assert_eq!(r.failure_node.as_deref(), None);
        assert!(r.cache_hit);
        assert_eq!(r.steps[0].details, "bypassed");
        assert_eq!(r.steps[1].details, "bypassed");
        assert_eq!(r.steps[2].details, "bypassed");
        assert_eq!(r.steps[3].details, "bypassed");
        assert_eq!(r.steps[4].details, "hit");
        assert_eq!(r.steps[5].details, "redirect");
    }

    #[tokio::test]
    async fn playback_trace_cache_miss_success() {
        diagnostics::init_diagnostics();
        diagnostics::clean_diagnostics();

        let mut trace = PlaybackDiagnosisTrace::new("server-1", "item-cache-miss", "UA");
        trace.push_step(STEP_EMBY_PATH, Instant::now(), "matched");
        trace.push_step(STEP_MAPPING, Instant::now(), "matched");
        trace.bypass_steps(&[STEP_OPENLIST, STEP_RAW_URL]);
        trace.push_step(STEP_CACHE, Instant::now(), "miss");
        trace.set_cache_hit(false);
        trace.push_step(STEP_REDIRECT, Instant::now(), "redirect");
        trace.finish().await;

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let records = diagnostics::get_diagnosis_records();
        let r = records
            .iter()
            .find(|r| r.media_id == "item-cache-miss")
            .expect("record");
        assert_eq!(
            r.steps.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            vec!["Emby Path", "映射", "OpenList", "raw_url", "缓存", "302"]
        );
        assert_eq!(r.failure_node.as_deref(), None);
        assert!(!r.cache_hit);
        assert_eq!(r.steps[0].details, "matched");
        assert_eq!(r.steps[1].details, "matched");
        assert_eq!(r.steps[2].details, "bypassed");
        assert_eq!(r.steps[3].details, "bypassed");
        assert_eq!(r.steps[4].details, "miss");
        assert_eq!(r.steps[5].details, "redirect");
    }

    #[tokio::test]
    async fn playback_trace_mapping_failure() {
        diagnostics::init_diagnostics();
        diagnostics::clean_diagnostics();

        let mut trace = PlaybackDiagnosisTrace::new("server-1", "item-map-fail", "UA");
        trace.push_step(STEP_EMBY_PATH, Instant::now(), "matched");
        trace.fail_step(STEP_MAPPING, Instant::now(), "failed");
        trace.finish().await;

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let records = diagnostics::get_diagnosis_records();
        let r = records
            .iter()
            .find(|r| r.media_id == "item-map-fail")
            .expect("record");
        assert_eq!(r.failure_node.as_deref(), Some("映射"));
        assert_eq!(
            r.steps.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            vec!["Emby Path", "映射"]
        );
    }

    #[tokio::test]
    async fn playback_trace_openlist_failure() {
        diagnostics::init_diagnostics();
        diagnostics::clean_diagnostics();

        let mut trace = PlaybackDiagnosisTrace::new("server-1", "item-ol-fail", "UA");
        trace.push_step(STEP_EMBY_PATH, Instant::now(), "matched");
        trace.push_step(STEP_MAPPING, Instant::now(), "matched");
        trace.fail_step(STEP_OPENLIST, Instant::now(), "failed");
        trace.finish().await;

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let records = diagnostics::get_diagnosis_records();
        let r = records
            .iter()
            .find(|r| r.media_id == "item-ol-fail")
            .expect("record");
        assert_eq!(r.failure_node.as_deref(), Some("OpenList"));
        assert_eq!(
            r.steps.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            vec!["Emby Path", "映射", "OpenList"]
        );
    }

    #[tokio::test]
    async fn playback_trace_raw_url_failure() {
        diagnostics::init_diagnostics();
        diagnostics::clean_diagnostics();

        let mut trace = PlaybackDiagnosisTrace::new("server-1", "item-raw-fail", "UA");
        trace.push_step(STEP_EMBY_PATH, Instant::now(), "matched");
        trace.push_step(STEP_MAPPING, Instant::now(), "matched");
        trace.push_step(STEP_OPENLIST, Instant::now(), "matched");
        trace.fail_step(STEP_RAW_URL, Instant::now(), "failed");
        trace.finish().await;

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let records = diagnostics::get_diagnosis_records();
        let r = records
            .iter()
            .find(|r| r.media_id == "item-raw-fail")
            .expect("record");
        assert_eq!(r.failure_node.as_deref(), Some("raw_url"));
        assert_eq!(
            r.steps.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            vec!["Emby Path", "映射", "OpenList", "raw_url"]
        );
    }

    #[tokio::test]
    async fn playback_trace_fallback_non_target_branch() {
        diagnostics::init_diagnostics();
        diagnostics::clean_diagnostics();

        let mut trace = PlaybackDiagnosisTrace::new("server-1", "item-fallback", "UA");
        trace.push_step(STEP_EMBY_PATH, Instant::now(), "matched");
        trace.bypass_steps(&[STEP_MAPPING, STEP_OPENLIST, STEP_RAW_URL]);
        trace.push_step(STEP_REDIRECT, Instant::now(), "proxy");
        trace.finish().await;

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let records = diagnostics::get_diagnosis_records();
        let r = records
            .iter()
            .find(|r| r.media_id == "item-fallback")
            .expect("record");
        assert_eq!(
            r.steps.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            vec!["Emby Path", "映射", "OpenList", "raw_url", "302"]
        );
        assert_eq!(r.failure_node.as_deref(), None);
        assert_eq!(r.steps[0].details, "matched");
        assert_eq!(r.steps[1].details, "bypassed");
        assert_eq!(r.steps[2].details, "bypassed");
        assert_eq!(r.steps[3].details, "bypassed");
        assert_eq!(r.steps[4].details, "proxy");
    }

    #[tokio::test]
    async fn playback_trace_first_failure_blocks_later_success() {
        diagnostics::init_diagnostics();
        diagnostics::clean_diagnostics();

        let mut trace = PlaybackDiagnosisTrace::new("server-1", "item-first-fail", "UA");
        trace.push_step(STEP_EMBY_PATH, Instant::now(), "matched");
        trace.fail_step(STEP_MAPPING, Instant::now(), "failed");
        trace.push_step(STEP_OPENLIST, Instant::now(), "matched");
        trace.push_step(STEP_RAW_URL, Instant::now(), "matched");
        trace.push_step(STEP_CACHE, Instant::now(), "hit");
        trace.set_cache_hit(true);
        trace.push_step(STEP_REDIRECT, Instant::now(), "redirect");
        trace.finish().await;

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let records = diagnostics::get_diagnosis_records();
        let r = records
            .iter()
            .find(|r| r.media_id == "item-first-fail")
            .expect("record");
        assert_eq!(r.failure_node.as_deref(), Some("映射"));
        assert_eq!(
            r.steps.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            vec!["Emby Path", "映射"]
        );
    }

    #[tokio::test]
    async fn playback_trace_drop_records_sanitized_cancellation() {
        diagnostics::init_diagnostics();
        diagnostics::clean_diagnostics();

        let trace_future = async {
            let mut trace = PlaybackDiagnosisTrace::new(
                "server-1",
                "item-cancel",
                "Player/1.0 token=ua-secret",
            );
            trace.begin_step(
                STEP_OPENLIST,
                "fs_get path=/movie?api_key=upstream-secret token=body-secret",
            );
            std::future::pending::<()>().await;
            drop(trace);
        };
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(5), trace_future)
                .await
                .is_err()
        );

        let record = diagnostics::get_diagnosis_records()
            .into_iter()
            .find(|record| record.media_id == "item-cancel")
            .expect("cancellation diagnosis record");
        assert_eq!(record.failure_node.as_deref(), Some(STEP_OPENLIST));
        assert!(record.steps[0].details.contains("cancelled"));
        assert_diagnosis_timing(&record);
        let json = serde_json::to_string(&record).unwrap();
        for secret in ["upstream-secret", "body-secret", "ua-secret"] {
            assert!(!json.contains(secret), "secret leaked: {json}");
        }
    }

    #[tokio::test]
    async fn playback_trace_keeps_server_and_media_scopes_isolated() {
        diagnostics::init_diagnostics();
        diagnostics::clean_diagnostics();

        let mut first = PlaybackDiagnosisTrace::new("server-1", "media-1", "UA-1");
        first.push_step(STEP_EMBY_PATH, Instant::now(), "matched");
        first.push_step(STEP_REDIRECT, Instant::now(), "redirect");
        first.finish().await;
        let mut second = PlaybackDiagnosisTrace::new("server-2", "media-1", "UA-2");
        second.push_step(STEP_EMBY_PATH, Instant::now(), "matched");
        second.push_step(STEP_REDIRECT, Instant::now(), "redirect");
        second.finish().await;
        let mut third = PlaybackDiagnosisTrace::new("server-1", "media-2", "UA-3");
        third.push_step(STEP_EMBY_PATH, Instant::now(), "matched");
        third.push_step(STEP_REDIRECT, Instant::now(), "redirect");
        third.finish().await;

        let records = diagnostics::get_diagnosis_records();
        assert!(records.iter().any(|record| {
            record.server_id == "server-1" && record.media_id == "media-1" && record.ua == "UA-1"
        }));
        assert!(records.iter().any(|record| {
            record.server_id == "server-2" && record.media_id == "media-1" && record.ua == "UA-2"
        }));
        assert!(records.iter().any(|record| {
            record.server_id == "server-1" && record.media_id == "media-2" && record.ua == "UA-3"
        }));
    }
}
