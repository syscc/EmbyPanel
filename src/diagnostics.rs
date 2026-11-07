//! Playback diagnosis collector, record construction, and security sanitization.
//!
//! Security boundary: every record that enters the buffer or leaves via the HTTP API
//! must pass through [`sanitize_diagnosis_record`]. Callers must still avoid feeding
//! raw credentials when possible; sanitization is defense-in-depth, not a license to
//! log secrets.

use serde::{Deserialize, Serialize};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Stable placeholder substituted for secret values.
pub const REDACTED_PLACEHOLDER: &str = "[REDACTED]";

/// Maximum number of diagnosis records retained in memory.
pub const DIAGNOSTICS_CAPACITY: usize = 1000;

const MAX_DETAIL_CHARS: usize = 1024;
const MAX_ID_CHARS: usize = 128;
const MAX_UA_CHARS: usize = 512;
const MAX_STEP_NAME_CHARS: usize = 64;
const MAX_QUERY_VALUE_CHARS: usize = 256;

const SENSITIVE_KEYS: &[&str] = &[
    "api_key",
    "api-key",
    "apikey",
    "access_token",
    "access-token",
    "refresh_token",
    "token",
    "password",
    "passwd",
    "authorization",
    "cookie",
    "set-cookie",
    "x-emby-token",
    "x-mediabrowser-token",
    "x-emby-authorization",
    "x-mediabrowser-authorization",
    "secret",
    "client_secret",
    "credential",
    "sign",
    "signature",
    "sig",
    "x-amz-signature",
    "x-amz-credential",
    "x-amz-security-token",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnosisRecord {
    pub request_id: String,
    pub server_id: String,
    pub media_id: String,
    pub ua: String,
    pub steps: Vec<DiagnosisStep>,
    pub cache_hit: bool,
    pub failure_node: Option<String>,
    pub sanitized_errors: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnosisStep {
    pub name: String,
    pub duration_ms: u64,
    pub details: String,
}

#[derive(Clone)]
pub struct DiagnosisCollector {
    buf: Arc<Mutex<Vec<DiagnosisRecord>>>,
    capacity: usize,
}

impl DiagnosisCollector {
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            buf: Arc::new(Mutex::new(Vec::with_capacity(capacity))),
            capacity,
        }
    }

    pub fn get_records(&self) -> Vec<DiagnosisRecord> {
        // Safe snapshot under short lock; does not drain retention buffer.
        let buf = self.buf.lock().unwrap();
        buf.clone()
    }

    pub fn clear(&self) {
        let mut buf = self.buf.lock().unwrap();
        buf.clear();
    }

    pub async fn record(&self, record: DiagnosisRecord) {
        self.ingest(record);
    }

    fn ingest(&self, record: DiagnosisRecord) {
        // Sanitize at the collector edge so raw records cannot bypass
        // create_diagnosis_record. The write is synchronous so a successful
        // record().await is immediately visible and `new()` does not need a runtime.
        let record = sanitize_diagnosis_record(record);
        let evicted = {
            let mut guard = self.buf.lock().unwrap();
            // Retain the newest records instead of draining the whole buffer when full.
            let evicted = if guard.len() >= self.capacity {
                Some(guard.remove(0))
            } else {
                None
            };
            guard.push(record);
            evicted
        };
        if let Some(record) = evicted {
            tracing::info!(
                ?record,
                "playback diagnosis record evicted from retention buffer"
            );
        }
    }
}

static COLLECTOR: OnceLock<DiagnosisCollector> = OnceLock::new();
static REQUEST_ID_COUNTER: AtomicU64 = AtomicU64::new(0);

#[cfg(test)]
thread_local! {
    // Parallel cargo tests share the process; isolate the global collector per test thread.
    static TEST_COLLECTOR: DiagnosisCollector = DiagnosisCollector::new(DIAGNOSTICS_CAPACITY);
}

fn with_active_collector<T>(f: impl FnOnce(&DiagnosisCollector) -> T) -> Option<T> {
    #[cfg(test)]
    {
        return Some(TEST_COLLECTOR.with(f));
    }
    #[cfg(not(test))]
    {
        COLLECTOR.get().map(f)
    }
}

pub fn init_diagnostics() {
    #[cfg(test)]
    {
        TEST_COLLECTOR.with(|_| ());
    }
    let _ = COLLECTOR.set(DiagnosisCollector::new(DIAGNOSTICS_CAPACITY));
}

pub async fn record_diagnosis(record: DiagnosisRecord) {
    record_diagnosis_now(record);
}

/// Record a diagnosis synchronously. This is used by cancellation/Drop paths where
/// awaiting is impossible, while retaining the same collector-edge sanitization.
pub fn record_diagnosis_now(record: DiagnosisRecord) {
    if let Some(collector) = with_active_collector(DiagnosisCollector::clone) {
        collector.ingest(record);
    }
}

pub fn get_diagnosis_records() -> Vec<DiagnosisRecord> {
    with_active_collector(DiagnosisCollector::get_records).unwrap_or_default()
}

pub fn clean_diagnostics() {
    let _ = with_active_collector(DiagnosisCollector::clear);
}

/// Build a diagnosis record and run it through the security sanitizer.
///
/// Future playback-chain hook (proxy/main — do **not** pass raw secrets):
/// ```ignore
/// let record = diagnostics::create_diagnosis_record(
///     server_id,   // already an id, not a URL
///     media_id,
///     ua,          // user-agent only
///     steps,       // details may contain URLs; sanitizer redacts secrets
///     cache_hit,
///     failure_node,
/// );
/// diagnostics::record_diagnosis(record).await;
/// // Or: diagnostics::sanitize_diagnostic_text(raw_detail) before building steps.
/// ```
pub fn create_diagnosis_record(
    server_id: &str,
    media_id: &str,
    ua: &str,
    steps: Vec<DiagnosisStep>,
    cache_hit: bool,
    failure_node: Option<String>,
) -> DiagnosisRecord {
    let request_id = format!(
        "diag-{}",
        REQUEST_ID_COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    sanitize_diagnosis_record(DiagnosisRecord {
        request_id,
        server_id: server_id.to_string(),
        media_id: media_id.to_string(),
        ua: ua.to_string(),
        steps,
        cache_hit,
        failure_node,
        sanitized_errors: Vec::new(),
    })
}

/// Full-record sanitizer used at construction, ingest, and API egress.
pub fn sanitize_diagnosis_record(mut record: DiagnosisRecord) -> DiagnosisRecord {
    record.request_id = normalize_id_like(&record.request_id);
    record.server_id = normalize_id_like(&record.server_id);
    record.media_id = normalize_id_like(&record.media_id);
    record.ua = truncate_chars(&sanitize_diagnostic_text(&record.ua), MAX_UA_CHARS);
    record.failure_node = record
        .failure_node
        .map(|node| normalize_id_like(&node))
        .filter(|node| !node.is_empty());
    record.steps = record
        .steps
        .into_iter()
        .map(sanitize_diagnosis_step)
        .collect();
    record.sanitized_errors = record
        .failure_node
        .as_deref()
        .and_then(|failure_node| record.steps.iter().find(|step| step.name == failure_node))
        .map(|step| vec![format!("{}: {}", step.name, step.details)])
        .unwrap_or_default();
    record
}

/// Sanitize a single step (name + details).
pub fn sanitize_diagnosis_step(mut step: DiagnosisStep) -> DiagnosisStep {
    step.name = normalize_step_name(&step.name);
    step.details = sanitize_diagnostic_text(&step.details);
    step
}

/// Case-insensitive, structure-aware redaction for free-form diagnostic text.
///
/// Rules:
/// - Strip control characters.
/// - Sanitize embedded `http(s)` URLs: drop userinfo, redact sensitive query values.
/// - Redact header-style and key/value credentials (Authorization, Cookie, api_key, …).
/// - Redact Bearer/Basic schemes and JSON string values for sensitive keys.
/// - Preserve non-sensitive media identifiers and surrounding context.
/// - Enforce max detail length.
pub fn sanitize_diagnostic_text(input: &str) -> String {
    let stripped: String = input.chars().filter(|ch| !ch.is_control()).collect();
    if stripped.is_empty() {
        return String::new();
    }
    let with_urls = sanitize_urls_in_text(&stripped);
    let with_creds = redact_credential_patterns(&with_urls);
    truncate_chars(&with_creds, MAX_DETAIL_CHARS)
}

/// Backward-compatible step mutator; prefer [`sanitize_diagnosis_step`].
pub fn redact_sensitive_info(steps: &mut Vec<DiagnosisStep>) {
    for step in steps.iter_mut() {
        *step = sanitize_diagnosis_step(std::mem::replace(
            step,
            DiagnosisStep {
                name: String::new(),
                duration_ms: 0,
                details: String::new(),
            },
        ));
    }
}

fn normalize_id_like(value: &str) -> String {
    // IDs normally come from trusted configuration or Emby paths, but the
    // collector/API boundary must not leak a credential if a caller supplies
    // a URL-like or otherwise untrusted identifier.
    let cleaned = sanitize_diagnostic_text(value).trim().to_string();
    truncate_chars(&cleaned, MAX_ID_CHARS)
}

fn normalize_step_name(value: &str) -> String {
    let cleaned: String = value
        .chars()
        .filter(|ch| !ch.is_control())
        .collect::<String>()
        .trim()
        .to_string();
    let cleaned = if cleaned.is_empty() {
        "step".to_string()
    } else {
        cleaned
    };
    truncate_chars(&cleaned, MAX_STEP_NAME_CHARS)
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    value.chars().take(max_chars).collect()
}

fn is_sensitive_key(key: &str) -> bool {
    SENSITIVE_KEYS
        .iter()
        .any(|sensitive| key.eq_ignore_ascii_case(sensitive))
}

fn sanitize_urls_in_text(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut idx = 0;
    while idx < input.len() {
        if let Some(rel) = find_url_start(&input[idx..]) {
            let abs = idx + rel;
            out.push_str(&input[idx..abs]);
            let rest = &input[abs..];
            let url_len = rest
                .find(|ch: char| {
                    ch.is_whitespace() || matches!(ch, ')' | '"' | '\'' | '<' | '>' | ',')
                })
                .unwrap_or(rest.len());
            let url_part = &rest[..url_len];
            if let Some(redacted) = sanitize_url_str(url_part) {
                out.push_str(&redacted);
            } else {
                out.push_str(url_part);
            }
            idx = abs + url_len;
        } else {
            out.push_str(&input[idx..]);
            break;
        }
    }
    out
}

fn find_url_start(input: &str) -> Option<usize> {
    let lower = input.to_ascii_lowercase();
    let http = lower.find("http://");
    let https = lower.find("https://");
    match (http, https) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

fn sanitize_url_str(value: &str) -> Option<String> {
    let mut parsed = url::Url::parse(value).ok()?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return None;
    }
    let _ = parsed.set_username("");
    let _ = parsed.set_password(None);
    if let Some(query) = parsed.query() {
        let mut pairs: Vec<(String, String)> = Vec::new();
        for (key, val) in url::form_urlencoded::parse(query.as_bytes()) {
            let value = if is_sensitive_key(&key) {
                REDACTED_PLACEHOLDER.to_string()
            } else {
                truncate_chars(&val, MAX_QUERY_VALUE_CHARS)
            };
            pairs.push((key.into_owned(), value));
        }
        if pairs.is_empty() {
            parsed.set_query(None);
        } else {
            // Build manually so the placeholder stays readable.
            let rebuilt = pairs
                .into_iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join("&");
            parsed.set_query(Some(&rebuilt));
        }
    }
    parsed.set_fragment(None);
    Some(parsed.to_string())
}

fn redact_credential_patterns(input: &str) -> String {
    let mut out = replace_json_sensitive_strings(input);
    out = replace_header_or_kv_secrets(&out);
    replace_auth_schemes(&out)
}

fn replace_json_sensitive_strings(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '"' {
            if let Some((key, key_end)) = scan_json_string(&chars, i) {
                let mut j = key_end;
                while j < chars.len() && chars[j].is_whitespace() {
                    j += 1;
                }
                if j < chars.len() && chars[j] == ':' {
                    j += 1;
                    while j < chars.len() && chars[j].is_whitespace() {
                        j += 1;
                    }
                    if j < chars.len() && chars[j] == '"' {
                        if let Some((_val, val_end)) = scan_json_string(&chars, j) {
                            out.push('"');
                            out.push_str(&key);
                            out.push('"');
                            out.push(':');
                            if is_sensitive_key(&key) {
                                out.push('"');
                                out.push_str(REDACTED_PLACEHOLDER);
                                out.push('"');
                            } else {
                                for ch in &chars[j..val_end] {
                                    out.push(*ch);
                                }
                            }
                            i = val_end;
                            continue;
                        }
                    }
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

fn scan_json_string(chars: &[char], start: usize) -> Option<(String, usize)> {
    if start >= chars.len() || chars[start] != '"' {
        return None;
    }
    let mut i = start + 1;
    let mut content = String::new();
    while i < chars.len() {
        match chars[i] {
            '\\' if i + 1 < chars.len() => {
                content.push(chars[i + 1]);
                i += 2;
            }
            '"' => return Some((content, i + 1)),
            ch => {
                content.push(ch);
                i += 1;
            }
        }
    }
    None
}

fn replace_header_or_kv_secrets(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let lower = input.to_ascii_lowercase();
    let mut idx = 0;
    while idx < input.len() {
        if let Some(key_len) = match_sensitive_key_at(&lower, idx) {
            if idx > 0 {
                let prev = lower.as_bytes()[idx - 1];
                if prev.is_ascii_alphanumeric() || prev == b'_' || prev == b'-' {
                    let ch = input[idx..].chars().next().unwrap();
                    out.push(ch);
                    idx += ch.len_utf8();
                    continue;
                }
            }
            let after_key = idx + key_len;
            let rest = &input[after_key..];
            let trimmed = rest.trim_start();
            let ws = rest.len() - trimmed.len();
            if let Some(sep) = trimmed.chars().next().filter(|c| *c == ':' || *c == '=') {
                let after_sep = &trimmed[sep.len_utf8()..];
                let value_ws_len = after_sep.len() - after_sep.trim_start().len();
                let value = after_sep.trim_start();
                if !value.is_empty() {
                    let value_len = value_len_for(sep, value);
                    // Preserve original key casing.
                    out.push_str(&input[idx..after_key]);
                    out.push_str(&input[after_key..after_key + ws]);
                    out.push(sep);
                    out.push_str(&after_sep[..value_ws_len]);
                    out.push_str(REDACTED_PLACEHOLDER);
                    idx = after_key + ws + sep.len_utf8() + value_ws_len + value_len;
                    continue;
                }
            }
        }
        let ch = input[idx..].chars().next().unwrap();
        out.push(ch);
        idx += ch.len_utf8();
    }
    out
}

fn match_sensitive_key_at(lower: &str, idx: usize) -> Option<usize> {
    let slice = &lower[idx..];
    let mut best: Option<usize> = None;
    for key in SENSITIVE_KEYS {
        if slice.starts_with(key) {
            let len = key.len();
            let ok_right = slice
                .as_bytes()
                .get(len)
                .map(|b| {
                    let c = *b as char;
                    !(c.is_ascii_alphanumeric() || c == '_' || c == '-')
                })
                .unwrap_or(true);
            if ok_right && best.map(|l| len > l).unwrap_or(true) {
                best = Some(len);
            }
        }
    }
    best
}

fn value_len_for(sep: char, value: &str) -> usize {
    if sep == ':' {
        // Header-style values may contain spaces (Bearer xxx). Stop at list separators.
        value
            .char_indices()
            .find(|(_, ch)| *ch == ';' || *ch == '\n' || *ch == '\r')
            .map(|(i, _)| i)
            .unwrap_or(value.len())
    } else if let Some(label_len) = auth_scheme_label_len(value) {
        // `Authorization=Bearer <token>` must redact scheme+token, not just "Bearer".
        let after = &value[label_len..];
        let ws = after.len() - after.trim_start().len();
        let token = after.trim_start();
        if token.is_empty() {
            value_token_len(value)
        } else {
            label_len + ws + value_token_len(token)
        }
    } else {
        value_token_len(value)
    }
}

fn auth_scheme_label_len(value: &str) -> Option<usize> {
    let lower = value.to_ascii_lowercase();
    if lower.starts_with("bearer ") {
        Some(6)
    } else if lower.starts_with("basic ") {
        Some(5)
    } else {
        None
    }
}

fn value_token_len(value: &str) -> usize {
    // Keep an already-substituted placeholder intact so `]` is not treated as a
    // value terminator (`token=[REDACTED]` must stay idempotent).
    if value.starts_with(REDACTED_PLACEHOLDER) {
        return REDACTED_PLACEHOLDER.len();
    }
    value
        .char_indices()
        .find(|(_, ch)| {
            ch.is_whitespace()
                || matches!(
                    ch,
                    ',' | ';' | '&' | '"' | '\'' | '<' | '>' | ')' | ']' | '{' | '}'
                )
        })
        .map(|(i, _)| i)
        .unwrap_or(value.len())
}

fn replace_auth_schemes(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let lower = input.to_ascii_lowercase();
    let mut idx = 0;
    while idx < input.len() {
        let rest_lower = &lower[idx..];
        let scheme = if rest_lower.starts_with("bearer ") {
            Some(6usize)
        } else if rest_lower.starts_with("basic ") {
            Some(5usize)
        } else {
            None
        };
        if let Some(label_len) = scheme {
            let boundary_ok = idx == 0 || {
                let prev = lower.as_bytes()[idx - 1];
                !prev.is_ascii_alphanumeric() && prev != b'_'
            };
            if boundary_ok {
                let after = &input[idx + label_len..];
                let after_ws = after.trim_start();
                let ws = after.len() - after_ws.len();
                if !after_ws.is_empty() {
                    let token_len = value_token_len(after_ws);
                    out.push_str(&input[idx..idx + label_len]);
                    out.push_str(&after[..ws]);
                    out.push_str(REDACTED_PLACEHOLDER);
                    idx = idx + label_len + ws + token_len;
                    continue;
                }
            }
        }
        let ch = input[idx..].chars().next().unwrap();
        out.push(ch);
        idx += ch.len_utf8();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_record(id: &str, server: &str) -> DiagnosisRecord {
        DiagnosisRecord {
            request_id: id.to_string(),
            server_id: server.to_string(),
            media_id: "m1".to_string(),
            ua: "ua".to_string(),
            steps: vec![DiagnosisStep {
                name: "openlist".to_string(),
                duration_ms: 1,
                details: "ok".to_string(),
            }],
            cache_hit: false,
            failure_node: None,
            sanitized_errors: vec![],
        }
    }

    #[test]
    fn creates_diagnosis_record() {
        let steps = vec![DiagnosisStep {
            name: "openlist".to_string(),
            duration_ms: 10,
            details: "ok".to_string(),
        }];
        let record = create_diagnosis_record("server1", "media1", "test-ua", steps, false, None);
        assert!(record.request_id.starts_with("diag-"));
        assert_eq!(record.server_id, "server1");
        assert_eq!(record.media_id, "media1");
        assert_eq!(record.steps[0].details, "ok");
    }

    #[test]
    fn step_order_is_correct() {
        let steps = vec![
            DiagnosisStep {
                name: "openlist".to_string(),
                duration_ms: 10,
                details: "ok".to_string(),
            },
            DiagnosisStep {
                name: "cache".to_string(),
                duration_ms: 5,
                details: "miss".to_string(),
            },
        ];
        let record = create_diagnosis_record("s", "m", "u", steps, false, None);
        assert_eq!(record.steps[0].name, "openlist");
        assert_eq!(record.steps[1].name, "cache");
    }

    #[test]
    fn redacts_case_variants_and_preserves_context() {
        let mut steps = vec![
            DiagnosisStep {
                name: "openlist".to_string(),
                duration_ms: 10,
                details: "API_KEY=secret123 item=abc".to_string(),
            },
            DiagnosisStep {
                name: "cache".to_string(),
                duration_ms: 5,
                details: "status=ok media_id=42".to_string(),
            },
        ];
        redact_sensitive_info(&mut steps);
        assert!(steps[0].details.contains(REDACTED_PLACEHOLDER));
        assert!(!steps[0].details.contains("secret123"));
        assert!(steps[0].details.contains("item=abc"));
        assert_eq!(steps[1].details, "status=ok media_id=42");
    }

    #[test]
    fn redacts_authorization_bearer_basic_cookie_and_json() {
        let details = concat!(
            "Authorization: Bearer super-secret-token; ",
            "Cookie: session=abc123def; ",
            "Basic dXNlcjpwYXNz; ",
            r#"{"api_key":"nested-secret","media":"ok"}"#
        );
        let record = create_diagnosis_record(
            "s1",
            "m1",
            "ua",
            vec![DiagnosisStep {
                name: "auth".to_string(),
                duration_ms: 1,
                details: details.to_string(),
            }],
            false,
            Some("auth".to_string()),
        );
        let json = serde_json::to_string(&record).expect("serialize non-empty dto");
        assert!(!json.contains("super-secret-token"), "{json}");
        assert!(!json.contains("session=abc123def"), "{json}");
        assert!(!json.contains("dXNlcjpwYXNz"), "{json}");
        assert!(!json.contains("nested-secret"), "{json}");
        assert!(json.contains(REDACTED_PLACEHOLDER), "{json}");
        assert!(json.contains("media"), "{json}");
        assert!(!record.sanitized_errors.is_empty());
    }

    #[test]
    fn redacts_url_userinfo_and_query_tokens() {
        let details = "fetch https://user:pass@cdn.example.com/play.m3u8?api_key=leak&quality=1080 token=also-leak";
        let sanitized = sanitize_diagnostic_text(details);
        assert!(
            !sanitized.contains("user:pass"),
            "userinfo leaked: {sanitized}"
        );
        assert!(!sanitized.contains("leak"), "secret leaked: {sanitized}");
        assert!(!sanitized.contains("also-leak"), "{sanitized}");
        assert!(
            sanitized.contains("quality=1080") || sanitized.contains("cdn.example.com"),
            "over-redacted non-sensitive context: {sanitized}"
        );
        assert!(sanitized.contains(REDACTED_PLACEHOLDER), "{sanitized}");
    }

    #[test]
    fn combined_sensitive_text_serializes_without_secrets() {
        let steps = vec![DiagnosisStep {
            name: "proxy".to_string(),
            duration_ms: 3,
            details: concat!(
                "PASSWD=p@ss; Set-Cookie: cookieval; ",
                "https://alice:bob@host/item?Token=T0PSECRET&x-emby-token=E1SECRET&path=/Movies/1.mkv ",
                r#"Authorization=Bearer ZZZSECRET json={"Password":"hunter2"}"#
            )
            .to_string(),
        }];
        let record = create_diagnosis_record("srv", "media-9", "Emby/1.0", steps, true, None);
        let json = serde_json::to_string_pretty(&record).unwrap();
        for secret in [
            "p@ss",
            "alice:bob",
            "T0PSECRET",
            "E1SECRET",
            "ZZZSECRET",
            "hunter2",
            "cookieval",
        ] {
            assert!(!json.contains(secret), "secret `{secret}` in {json}");
        }
        assert!(json.contains("media-9"));
        assert!(json.contains("cache_hit"));
    }

    #[test]
    fn sanitizes_identifier_fields_and_failure_node() {
        let record = create_diagnosis_record(
            "https://diag.example.test/?api_key=request-secret",
            "https://media.example.test/item?token=media-secret",
            "UA",
            vec![],
            false,
            Some("https://node.example.test/?cookie=node-secret".into()),
        );
        let json = serde_json::to_string(&record).unwrap();
        for secret in ["request-secret", "media-secret", "node-secret"] {
            assert!(!json.contains(secret), "secret `{secret}` in {json}");
        }
        assert!(json.contains(REDACTED_PLACEHOLDER), "{json}");
    }

    #[test]
    fn strips_control_chars_and_clamps_length() {
        let dirty = format!("ok\u{0000}id\u{0007} {}", "x".repeat(MAX_DETAIL_CHARS + 50));
        let cleaned = sanitize_diagnostic_text(&dirty);
        assert!(!cleaned.contains('\0'));
        assert!(!cleaned.chars().any(|c| c.is_control()));
        assert!(cleaned.chars().count() <= MAX_DETAIL_CHARS);

        let record = create_diagnosis_record(
            "  server\u{0001} ",
            &"m".repeat(MAX_ID_CHARS + 10),
            "ua",
            vec![DiagnosisStep {
                name: format!("n\u{0008}"),
                duration_ms: 1,
                details: "fine".into(),
            }],
            false,
            Some(" node\u{0009} ".into()),
        );
        assert!(!record.server_id.contains('\u{0001}'));
        assert!(record.media_id.chars().count() <= MAX_ID_CHARS);
        assert!(!record.steps[0].name.contains('\u{0008}'));
    }

    #[test]
    fn does_not_over_redact_normal_media_context() {
        let text = "resolved item_id=122 path=/Movies/Foo.mkv cache=miss status=200";
        assert_eq!(sanitize_diagnostic_text(text), text);
    }

    #[tokio::test]
    async fn capacity_retains_newest_records_in_stable_order() {
        let collector = DiagnosisCollector::new(2);
        collector.record(sample_record("1", "s")).await;
        collector.record(sample_record("2", "s")).await;
        collector.record(sample_record("3", "s")).await;
        let snapshot = collector.get_records();
        assert_eq!(snapshot.len(), 2);
        assert_eq!(snapshot[0].request_id, "2");
        assert_eq!(snapshot[1].request_id, "3");
    }

    #[test]
    fn concurrent_snapshot_is_safe() {
        let collector = DiagnosisCollector::new(4);
        let snap1 = collector.get_records();
        let snap2 = collector.get_records();
        assert_eq!(snap1.len(), snap2.len());
        collector.clear();
        let snap3 = collector.get_records();
        assert!(snap3.is_empty());
    }

    #[test]
    fn sanitize_on_record_edge_is_idempotent() {
        let once = sanitize_diagnostic_text("token=abc Authorization: Bearer zzz");
        let twice = sanitize_diagnostic_text(&once);
        assert_eq!(once, twice);
        assert!(!twice.contains("abc"));
        assert!(!twice.contains("zzz"));
    }

    #[test]
    fn url_redaction_is_idempotent_across_record_boundaries() {
        let once = sanitize_diagnostic_text(
            "resolved https://cdn.example.test/movie.mkv?token=secret&quality=2160",
        );
        let twice = sanitize_diagnostic_text(&once);
        let thrice = sanitize_diagnostic_text(&twice);
        assert_eq!(once, twice);
        assert_eq!(twice, thrice);
        assert!(thrice.contains("token=[REDACTED]&quality=2160"));
        assert!(!thrice.contains("]]"));
        assert!(!thrice.contains("secret"));
    }

    #[test]
    fn successful_redacted_steps_are_not_reported_as_errors() {
        let record = create_diagnosis_record(
            "server",
            "media",
            "ua token=secret",
            vec![DiagnosisStep {
                name: "raw_url".to_string(),
                duration_ms: 1,
                details: "https://cdn.example.test/movie.mkv?token=secret".to_string(),
            }],
            false,
            None,
        );
        assert!(record.sanitized_errors.is_empty());
        assert!(record.steps[0].details.contains(REDACTED_PLACEHOLDER));
    }
}
