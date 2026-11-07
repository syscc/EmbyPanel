use moka::future::Cache;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::cmp::Ordering as CmpOrdering;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// Hard cap for management list/search responses.
pub const MAX_ENTRY_LIST: usize = 500;

/// Cache capacity is always measured in entry count, never bytes.
pub const CAPACITY_UNIT: &str = "entries";

// NUL distinguishes the versioned format from ordinary legacy server IDs.
const KEY_V1_PREFIX: &str = "\0direct-link:v1:";

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct CacheStats {
    /// Current live entry count (unit = entries, not bytes).
    pub capacity: usize,
    /// Sum of the effective capacities of the global fallback and server buckets.
    #[serde(rename = "maxCapacity")]
    pub max_capacity: u64,
    /// Always `"entries"` so clients do not treat capacity as bytes.
    pub unit: &'static str,
    /// Number of successful cache lookups since process/cache initialization.
    pub hits: usize,
    /// Number of unsuccessful cache lookups since process/cache initialization.
    pub misses: usize,
    #[serde(rename = "hitRate")]
    pub hit_rate: f64,
}

/// Safe public view of a cache entry. Never includes raw key, URL, token, or UA.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CacheEntryView {
    /// Opaque stable identifier derived from the internal key (not reversible to secrets).
    pub id: String,
    #[serde(rename = "serverId")]
    pub server_id: String,
    #[serde(rename = "mediaId")]
    pub media_id: String,
    #[serde(rename = "sourceId")]
    pub source_id: String,
    /// True when the internal key could not be parsed into server/media/source fields.
    pub legacy: bool,
}

/// Parsed components of an internal direct-link cache key.
///
/// Production keys use a versioned JSON array; the legacy colon-separated format
/// remains readable for internal callers. UA never appears in management views.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedCacheKey {
    pub server_id: String,
    pub media_id: String,
    pub source_id: String,
    /// Remainder (UA). Never exposed via management API.
    pub rest: String,
}

struct RankedEntry {
    view: CacheEntryView,
    raw_key: String,
}

impl RankedEntry {
    fn compare(left: &Self, right: &Self) -> CmpOrdering {
        left.view
            .server_id
            .cmp(&right.view.server_id)
            .then_with(|| left.view.media_id.cmp(&right.view.media_id))
            .then_with(|| left.view.source_id.cmp(&right.view.source_id))
            .then_with(|| left.view.id.cmp(&right.view.id))
            .then_with(|| left.raw_key.cmp(&right.raw_key))
    }
}

impl PartialEq for RankedEntry {
    fn eq(&self, other: &Self) -> bool {
        Self::compare(self, other) == CmpOrdering::Equal
    }
}

impl Eq for RankedEntry {}

impl PartialOrd for RankedEntry {
    fn partial_cmp(&self, other: &Self) -> Option<CmpOrdering> {
        Some(self.cmp(other))
    }
}

impl Ord for RankedEntry {
    fn cmp(&self, other: &Self) -> CmpOrdering {
        Self::compare(self, other)
    }
}

/// Runtime policy for one direct-link cache bucket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirectLinkCachePolicy {
    pub enabled: bool,
    pub ttl_seconds: u64,
    pub max_capacity: u64,
}

impl DirectLinkCachePolicy {
    pub const fn new(enabled: bool, ttl_seconds: u64, max_capacity: u64) -> Self {
        Self {
            enabled,
            ttl_seconds,
            max_capacity,
        }
    }

    fn is_effective(self) -> bool {
        self.enabled && self.ttl_seconds > 0 && self.max_capacity > 0
    }

    fn effective_capacity(self) -> u64 {
        if self.is_effective() {
            self.max_capacity
        } else {
            0
        }
    }
}

#[derive(Clone)]
struct CacheBucket {
    policy: DirectLinkCachePolicy,
    inner: Cache<String, String>,
}

impl CacheBucket {
    fn new(policy: DirectLinkCachePolicy) -> Self {
        let effective = policy.is_effective();
        let inner = Cache::builder()
            .time_to_live(Duration::from_secs(if effective {
                policy.ttl_seconds
            } else {
                1
            }))
            .max_capacity(if effective { policy.max_capacity } else { 1 })
            .build();
        Self { policy, inner }
    }

    fn is_enabled(&self) -> bool {
        self.policy.is_effective()
    }
}

#[derive(Clone)]
pub struct DirectLinkCache {
    /// Fallback bucket for unknown server ids and legacy/unstructured keys.
    default_bucket: CacheBucket,
    /// Explicit per-server buckets. Their policies override the default policy.
    server_buckets: Arc<HashMap<String, CacheBucket>>,
    hit_count: Arc<AtomicUsize>,
    miss_count: Arc<AtomicUsize>,
}

impl DirectLinkCache {
    /// Backward-compatible single-cache constructor.
    pub fn new(enabled: bool, ttl_seconds: u64, max_capacity: u64) -> Self {
        Self::new_with_server_policies(
            DirectLinkCachePolicy::new(enabled, ttl_seconds, max_capacity),
            std::iter::empty(),
        )
    }

    /// Builds explicit server buckets using the same policy as the global fallback.
    ///
    /// This is useful when callers know the server set but do not need overrides yet.
    /// Duplicate and empty server ids are ignored.
    pub fn new_with_strategy(
        enabled: bool,
        ttl_seconds: u64,
        max_capacity: u64,
        server_ids: Vec<String>,
    ) -> Self {
        let policy = DirectLinkCachePolicy::new(enabled, ttl_seconds, max_capacity);
        Self::new_with_server_policies(
            policy,
            server_ids.into_iter().map(|server_id| (server_id, policy)),
        )
    }

    /// Builds a global fallback bucket plus explicit per-server policy buckets.
    ///
    /// Structured keys route to a matching server bucket. Unknown server ids and
    /// legacy/unstructured keys use `default_policy`.
    pub fn new_with_server_policies<I>(
        default_policy: DirectLinkCachePolicy,
        server_policies: I,
    ) -> Self
    where
        I: IntoIterator<Item = (String, DirectLinkCachePolicy)>,
    {
        let server_buckets = server_policies
            .into_iter()
            .filter_map(|(server_id, policy)| {
                let server_id = server_id.trim().to_string();
                (!server_id.is_empty()).then(|| (server_id, CacheBucket::new(policy)))
            })
            .collect();

        Self {
            default_bucket: CacheBucket::new(default_policy),
            server_buckets: Arc::new(server_buckets),
            hit_count: Arc::new(AtomicUsize::new(0)),
            miss_count: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Returns the bucket selected for a parsed server id.
    /// Unknown server ids and `None` (legacy key) explicitly fall back globally.
    fn get_cache_for_server(&self, server_id: Option<&str>) -> &CacheBucket {
        server_id
            .and_then(|server_id| self.server_buckets.get(server_id))
            .unwrap_or(&self.default_bucket)
    }

    fn all_buckets(&self) -> impl Iterator<Item = &CacheBucket> {
        std::iter::once(&self.default_bucket).chain(self.server_buckets.values())
    }

    /// Build an unambiguous key even when server/media/source/UA contain colons.
    pub fn key(server_id: &str, media_id: &str, source_id: &str, user_agent: &str) -> String {
        let fields = serde_json::to_string(&[server_id, media_id, source_id, user_agent])
            .expect("string array serialization cannot fail");
        format!("{KEY_V1_PREFIX}{fields}")
    }

    /// Parse versioned keys and ordinary legacy colon-separated keys.
    pub fn parse_key(key: &str) -> Option<ParsedCacheKey> {
        let [server_id, media_id, source_id, rest] =
            if let Some(fields) = key.strip_prefix(KEY_V1_PREFIX) {
                serde_json::from_str::<[String; 4]>(fields).ok()?
            } else {
                let mut parts = key.splitn(4, ':');
                [
                    parts.next()?.to_string(),
                    parts.next()?.to_string(),
                    parts.next()?.to_string(),
                    parts.next()?.to_string(),
                ]
            };
        // Require non-empty structural fields; rest (UA) may be empty.
        if server_id.is_empty() || media_id.is_empty() || source_id.is_empty() {
            return None;
        }
        Some(ParsedCacheKey {
            server_id,
            media_id,
            source_id,
            rest,
        })
    }

    /// Opaque short id for management UI (sha256 prefix). Does not reveal URL/token/UA.
    pub fn opaque_id(key: &str) -> String {
        let digest = Sha256::digest(key.as_bytes());
        digest.iter().take(8).map(|b| format!("{b:02x}")).collect()
    }

    fn entry_view_from_parsed(key: &str, parsed: Option<ParsedCacheKey>) -> CacheEntryView {
        let id = Self::opaque_id(key);
        match parsed {
            Some(parsed) => CacheEntryView {
                id,
                server_id: parsed.server_id,
                media_id: parsed.media_id,
                source_id: parsed.source_id,
                legacy: false,
            },
            None => CacheEntryView {
                id,
                server_id: String::new(),
                media_id: String::new(),
                source_id: String::new(),
                legacy: true,
            },
        }
    }

    #[cfg(test)]
    fn entry_view(key: &str) -> CacheEntryView {
        Self::entry_view_from_parsed(key, Self::parse_key(key))
    }

    async fn current_keys(&self) -> Vec<String> {
        let mut keys = HashSet::new();
        for bucket in self.all_buckets() {
            bucket.inner.run_pending_tasks().await;
            keys.extend(bucket.inner.iter().map(|(key, _)| key.as_ref().clone()));
        }
        let mut keys: Vec<_> = keys.into_iter().collect();
        keys.sort();
        keys
    }

    fn matches_scope(key: &str, server_id: Option<&str>, media_id: Option<&str>) -> bool {
        match (server_id, media_id) {
            (Some(server_id), Some(media_id)) => Self::parse_key(key)
                .is_some_and(|parsed| parsed.server_id == server_id && parsed.media_id == media_id),
            (Some(server_id), None) => {
                Self::parse_key(key).is_some_and(|parsed| parsed.server_id == server_id)
            }
            (None, Some(media_id)) => {
                Self::parse_key(key).is_some_and(|parsed| parsed.media_id == media_id)
            }
            (None, None) => true,
        }
    }

    fn matches_search(view: &CacheEntryView, search: Option<&str>) -> bool {
        let Some(search) = search.map(str::trim).filter(|value| !value.is_empty()) else {
            return true;
        };
        let search = search.to_lowercase();
        view.id.to_lowercase().contains(&search)
            || view.server_id.to_lowercase().contains(&search)
            || view.media_id.to_lowercase().contains(&search)
            || view.source_id.to_lowercase().contains(&search)
    }

    #[cfg(test)]
    fn sort_view_entries(entries: &mut [(CacheEntryView, String)]) {
        entries.sort_by(|(left, left_key), (right, right_key)| {
            left.server_id
                .cmp(&right.server_id)
                .then_with(|| left.media_id.cmp(&right.media_id))
                .then_with(|| left.source_id.cmp(&right.source_id))
                .then_with(|| left.id.cmp(&right.id))
                .then_with(|| left_key.cmp(right_key))
        });
    }

    async fn collect_entry_views(
        &self,
        search: Option<&str>,
        requested_server_id: Option<&str>,
        allowed_server_ids: Option<&HashSet<String>>,
    ) -> Vec<CacheEntryView> {
        let mut entries = BinaryHeap::with_capacity(MAX_ENTRY_LIST);
        // Only retained keys need deduplication: once evicted, a key cannot beat
        // the monotonically improving top-K boundary when seen in another bucket.
        let mut retained_keys = HashSet::with_capacity(MAX_ENTRY_LIST);
        for bucket in self.all_buckets() {
            bucket.inner.run_pending_tasks().await;
            for (key, _) in bucket.inner.iter() {
                let raw_key = key.as_ref();
                let parsed = Self::parse_key(raw_key);
                let scoped_server_id = parsed
                    .as_ref()
                    .map_or("", |parsed| parsed.server_id.as_str());
                if requested_server_id.is_some_and(|server_id| scoped_server_id != server_id)
                    || allowed_server_ids
                        .is_some_and(|server_ids| !server_ids.contains(scoped_server_id))
                {
                    continue;
                }

                let view = Self::entry_view_from_parsed(raw_key, parsed);
                if !Self::matches_search(&view, search) {
                    continue;
                }
                if retained_keys.contains(raw_key) {
                    continue;
                }

                let candidate = RankedEntry {
                    view,
                    raw_key: raw_key.clone(),
                };
                if entries.len() < MAX_ENTRY_LIST {
                    retained_keys.insert(candidate.raw_key.clone());
                    entries.push(candidate);
                } else if entries.peek().is_some_and(|worst| candidate < *worst) {
                    let worst = entries.pop().expect("full heap has a worst entry");
                    retained_keys.remove(&worst.raw_key);
                    retained_keys.insert(candidate.raw_key.clone());
                    entries.push(candidate);
                }
            }
        }

        let mut entries = entries.into_vec();
        entries.sort_by(RankedEntry::compare);
        entries.into_iter().map(|entry| entry.view).collect()
    }

    pub async fn set(&self, key: String, value: String) {
        let parsed = Self::parse_key(&key);
        let bucket =
            self.get_cache_for_server(parsed.as_ref().map(|value| value.server_id.as_str()));
        if bucket.is_enabled() {
            bucket.inner.insert(key, value).await;
        }
    }

    pub async fn get(&self, key: &str) -> Option<String> {
        let parsed = Self::parse_key(key);
        let bucket =
            self.get_cache_for_server(parsed.as_ref().map(|value| value.server_id.as_str()));
        let value = if bucket.is_enabled() {
            bucket.inner.get(key).await
        } else {
            None
        };
        if value.is_some() {
            self.hit_count.fetch_add(1, Ordering::Relaxed);
        } else {
            self.miss_count.fetch_add(1, Ordering::Relaxed);
        }
        value
    }

    /// Backward-compatible raw-key search used by internal callers and tests.
    pub async fn search_entries(
        &self,
        server_id: Option<String>,
        media_id: Option<String>,
    ) -> Vec<String> {
        self.current_keys()
            .await
            .into_iter()
            .filter(|key| Self::matches_scope(key, server_id.as_deref(), media_id.as_deref()))
            .take(MAX_ENTRY_LIST)
            .collect()
    }

    /// Safe structured listing. Search never examines raw keys, values, URLs, tokens, or UA.
    pub async fn list_entry_views(&self, search: Option<&str>) -> Vec<CacheEntryView> {
        self.collect_entry_views(search, None, None).await
    }

    /// Safe authorized listing. Authorization is applied before the hard cap and pagination.
    pub async fn list_entry_views_page(
        &self,
        search: Option<&str>,
        requested_server_id: Option<&str>,
        allowed_server_ids: Option<&HashSet<String>>,
        start: usize,
        limit: usize,
    ) -> Vec<CacheEntryView> {
        self.collect_entry_views(search, requested_server_id, allowed_server_ids)
            .await
            .into_iter()
            .skip(start)
            .take(limit)
            .collect()
    }

    /// Removes entries matching the requested structured scope and returns the deleted count.
    pub async fn clean_by_server_media(
        &self,
        server_id: Option<String>,
        media_id: Option<String>,
    ) -> usize {
        let mut deleted = 0;
        for bucket in self.all_buckets() {
            bucket.inner.run_pending_tasks().await;
            let keys: Vec<_> = bucket
                .inner
                .iter()
                .map(|(key, _)| key.as_ref().clone())
                .filter(|key| Self::matches_scope(key, server_id.as_deref(), media_id.as_deref()))
                .collect();
            deleted += keys.len();
            for key in keys {
                bucket.inner.invalidate(&key).await;
            }
            bucket.inner.run_pending_tasks().await;
        }
        deleted
    }

    /// Compatibility wrapper for older internal callers.
    pub async fn clear(&self, server_id: Option<String>, media_id: Option<String>) {
        self.clean_by_server_media(server_id, media_id).await;
    }

    pub async fn get_stats(&self) -> CacheStats {
        let mut capacity = 0usize;
        let mut max_capacity = 0u64;
        for bucket in self.all_buckets() {
            bucket.inner.run_pending_tasks().await;
            capacity = capacity.saturating_add(bucket.inner.entry_count() as usize);
            max_capacity = max_capacity.saturating_add(bucket.policy.effective_capacity());
        }

        let hits = self.hit_count.load(Ordering::Relaxed);
        let misses = self.miss_count.load(Ordering::Relaxed);
        let total = hits.saturating_add(misses);
        let hit_rate = if total == 0 {
            0.0
        } else {
            hits as f64 / total as f64
        };
        CacheStats {
            capacity,
            max_capacity,
            unit: CAPACITY_UNIT,
            hits,
            misses,
            hit_rate,
        }
    }

    pub async fn invalidate_all(&self) {
        for bucket in self.all_buckets() {
            bucket.inner.invalidate_all();
            bucket.inner.run_pending_tasks().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use tokio::time::Duration as TokioDuration;

    fn key(server: &str, media: &str, source: &str, ua: &str) -> String {
        format!("{server}:{media}:{source}:{ua}")
    }

    fn policy(enabled: bool, ttl_seconds: u64, max_capacity: u64) -> DirectLinkCachePolicy {
        DirectLinkCachePolicy::new(enabled, ttl_seconds, max_capacity)
    }

    #[test]
    fn versioned_key_roundtrips_delimiters_and_preserves_legacy_parser() {
        let fields = ["a:b", "movie:\"电影", "source:one", "UA:token=private\n"];
        let encoded = DirectLinkCache::key(fields[0], fields[1], fields[2], fields[3]);
        let parsed = DirectLinkCache::parse_key(&encoded).unwrap();
        assert_eq!(parsed.server_id, fields[0]);
        assert_eq!(parsed.media_id, fields[1]);
        assert_eq!(parsed.source_id, fields[2]);
        assert_eq!(parsed.rest, fields[3]);
        assert_ne!(
            DirectLinkCache::key("a:b", "movie", "source", "UA"),
            DirectLinkCache::key("a", "b:movie", "source", "UA")
        );
        let legacy = DirectLinkCache::parse_key("a:movie:source:UA:extra").unwrap();
        assert_eq!(legacy.server_id, "a");
        assert_eq!(legacy.rest, "UA:extra");
        for fields in [
            ["", "movie", "source", ""],
            ["a", "", "source", ""],
            ["a", "movie", "", ""],
        ] {
            assert!(
                DirectLinkCache::parse_key(&DirectLinkCache::key(
                    fields[0], fields[1], fields[2], fields[3]
                ))
                .is_none()
            );
        }
        assert!(
            DirectLinkCache::parse_key(&format!("{KEY_V1_PREFIX}bad:movie:source:UA")).is_none()
        );
        assert!(
            DirectLinkCache::parse_key(&DirectLinkCache::key("a:b", "movie", "source", ""))
                .is_some()
        );
    }

    #[tokio::test]
    async fn colon_server_disabled_and_zero_policies_cannot_use_other_bucket() {
        for disabled in [
            policy(false, 60, 10),
            policy(true, 0, 10),
            policy(true, 60, 0),
        ] {
            for (server, other) in [("a:b", "a"), ("a", "a:b")] {
                let cache = DirectLinkCache::new_with_server_policies(
                    policy(true, 60, 10),
                    [
                        (server.to_string(), disabled),
                        (other.to_string(), policy(true, 60, 10)),
                    ],
                );
                let blocked = DirectLinkCache::key(server, "movie", "source", "UA:private");
                let allowed = DirectLinkCache::key(other, "movie", "source", "UA:private");
                cache.set(blocked.clone(), "blocked-url".into()).await;
                cache.set(allowed.clone(), "allowed-url".into()).await;
                assert_eq!(cache.get(&blocked).await, None);
                assert_eq!(cache.get(&allowed).await.as_deref(), Some("allowed-url"));
                assert_eq!(cache.get_stats().await.capacity, 1);
                assert_eq!(cache.list_entry_views(None).await[0].server_id, other);
            }
        }
    }

    #[tokio::test]
    async fn colon_server_listing_cleanup_and_legacy_keys_are_isolated() {
        let cache = DirectLinkCache::new_with_server_policies(
            policy(false, 0, 0),
            ["a", "a:b"].map(|server| (server.into(), policy(true, 60, 10))),
        );
        let colon = DirectLinkCache::key("a:b", "movie", "source", "private-UA:secret");
        let other_media = DirectLinkCache::key("a:b", "other", "source", "private-UA:secret");
        let plain = key("a", "movie", "source", "private-UA:secret");
        for raw in [&colon, &other_media, &plain] {
            cache
                .set(
                    raw.clone(),
                    "https://private-url.test/file?token=secret".into(),
                )
                .await;
        }
        let allowed = HashSet::from(["a:b".into()]);
        let views = cache
            .list_entry_views_page(None, None, Some(&allowed), 0, 10)
            .await;
        assert_eq!(views.len(), 2);
        assert!(
            views
                .iter()
                .all(|view| view.server_id == "a:b" && !view.legacy)
        );
        assert!(
            cache
                .list_entry_views_page(None, Some("a"), Some(&allowed), 0, 10)
                .await
                .is_empty()
        );
        let public = serde_json::to_string(&views).unwrap();
        assert!(!public.contains("private-UA"));
        assert!(!public.contains("private-url"));
        assert!(!public.contains("secret"));
        for search in ["private-UA", "private-url", "secret"] {
            assert!(cache.list_entry_views(Some(search)).await.is_empty());
        }
        assert_eq!(
            cache
                .search_entries(Some("a:b".into()), Some("movie".into()))
                .await,
            vec![colon.clone()]
        );
        assert_eq!(
            cache
                .clean_by_server_media(Some("a:b".into()), Some("movie".into()))
                .await,
            1
        );
        assert_eq!(cache.get(&colon).await, None);
        assert!(cache.get(&other_media).await.is_some());
        assert!(cache.get(&plain).await.is_some());
        assert_eq!(
            cache.clean_by_server_media(Some("a:b".into()), None).await,
            1
        );
        assert_eq!(cache.clean_by_server_media(Some("a".into()), None).await, 1);
        assert_eq!(cache.get_stats().await.capacity, 0);
    }

    #[tokio::test]
    async fn colon_server_ttl_and_capacity_are_isolated() {
        let cache = DirectLinkCache::new_with_server_policies(
            policy(false, 0, 0),
            [
                ("a:b".into(), policy(true, 1, 1)),
                ("a".into(), policy(true, 60, 3)),
            ],
        );
        for server in ["a:b", "a"] {
            for media in ["movie1", "movie2", "movie3"] {
                cache
                    .set(
                        DirectLinkCache::key(server, media, "source", "UA"),
                        "url".into(),
                    )
                    .await;
            }
        }
        assert_eq!(cache.get_stats().await.capacity, 4);
        assert_eq!(
            cache.search_entries(Some("a:b".into()), None).await.len(),
            1
        );
        assert_eq!(cache.search_entries(Some("a".into()), None).await.len(), 3);
        tokio::time::sleep(TokioDuration::from_millis(1_200)).await;
        assert_eq!(
            cache
                .get(&DirectLinkCache::key("a:b", "movie3", "source", "UA"))
                .await,
            None
        );
        assert_eq!(
            cache
                .get(&DirectLinkCache::key("a", "movie3", "source", "UA"))
                .await
                .as_deref(),
            Some("url")
        );
        assert!(
            cache
                .search_entries(Some("a:b".into()), None)
                .await
                .is_empty()
        );
        assert_eq!(cache.get_stats().await.capacity, 3);
        assert_eq!(
            cache
                .clean_by_server_media(None, Some("movie1".into()))
                .await,
            1
        );
        cache.invalidate_all().await;
        assert!(cache.list_entry_views(None).await.is_empty());
    }

    async fn legacy_view_page(
        cache: &DirectLinkCache,
        search: Option<&str>,
        requested_server_id: Option<&str>,
        allowed_server_ids: Option<&HashSet<String>>,
        start: usize,
        limit: usize,
    ) -> Vec<CacheEntryView> {
        let mut views: Vec<_> = cache
            .current_keys()
            .await
            .into_iter()
            .map(|key| DirectLinkCache::entry_view(&key))
            .filter(|view| DirectLinkCache::matches_search(view, search))
            .filter(|view| requested_server_id.is_none_or(|server_id| view.server_id == server_id))
            .filter(|view| {
                allowed_server_ids.is_none_or(|server_ids| server_ids.contains(&view.server_id))
            })
            .collect();
        views.sort_by(|left, right| {
            left.server_id
                .cmp(&right.server_id)
                .then_with(|| left.media_id.cmp(&right.media_id))
                .then_with(|| left.source_id.cmp(&right.source_id))
                .then_with(|| left.id.cmp(&right.id))
        });
        views
            .into_iter()
            .take(MAX_ENTRY_LIST)
            .skip(start)
            .take(limit)
            .collect()
    }

    #[tokio::test]
    async fn disable_and_zero_config_boundaries() {
        let cache = DirectLinkCache::new(false, 0, 0);
        let cache_key = key("server1", "media1", "default", "ua");
        cache.set(cache_key.clone(), "url1".to_string()).await;
        assert_eq!(cache.get(&cache_key).await, None);
        let stats = cache.get_stats().await;
        assert_eq!(stats.capacity, 0);
        assert_eq!(stats.hits, 0);
        assert_eq!(stats.misses, 1);
        assert_eq!(stats.hit_rate, 0.0);
        assert_eq!(stats.unit, CAPACITY_UNIT);
        assert_eq!(stats.max_capacity, 0);

        let zero_ttl = DirectLinkCache::new(true, 0, 100);
        zero_ttl.set(cache_key.clone(), "url1".to_string()).await;
        assert_eq!(zero_ttl.get(&cache_key).await, None);
        assert_eq!(zero_ttl.get_stats().await.max_capacity, 0);
    }

    #[tokio::test]
    async fn ttl_expire_cleanup() {
        let cache = DirectLinkCache::new(true, 1, 100);
        let cache_key = key("server1", "media1", "default", "ua");
        cache.set(cache_key, "url".to_string()).await;
        tokio::time::sleep(TokioDuration::from_millis(1_200)).await;
        assert!(cache.search_entries(None, None).await.is_empty());
        assert_eq!(cache.get_stats().await.capacity, 0);
    }

    #[tokio::test]
    async fn server_media_boundary() {
        let cache = DirectLinkCache::new(true, 60, 100);
        let key1 = key("server1", "media1", "default", "ua");
        let key10 = key("server10", "media10", "default", "ua");
        cache.set(key1.clone(), "url1".to_string()).await;
        cache.set(key10.clone(), "url10".to_string()).await;

        assert_eq!(
            cache
                .search_entries(Some("server1".to_string()), Some("media1".to_string()))
                .await,
            vec![key1.clone()]
        );
        assert!(
            cache
                .search_entries(Some("server1".to_string()), Some("media10".to_string()))
                .await
                .is_empty()
        );
        assert_eq!(
            cache
                .search_entries(Some("server10".to_string()), Some("media10".to_string()))
                .await,
            vec![key10]
        );
    }

    #[tokio::test]
    async fn global_disabled_server_enabled_and_legacy_fallback() {
        let cache = DirectLinkCache::new_with_server_policies(
            policy(false, 60, 100),
            [("enabled".to_string(), policy(true, 60, 10))],
        );
        let enabled_key = key("enabled", "media1", "default", "ua");
        let unknown_key = key("unknown", "media1", "default", "ua");
        let legacy_key = "legacy-key-without-structure".to_string();

        cache
            .set(enabled_key.clone(), "enabled-url".to_string())
            .await;
        cache
            .set(unknown_key.clone(), "unknown-url".to_string())
            .await;
        cache
            .set(legacy_key.clone(), "legacy-url".to_string())
            .await;

        assert_eq!(
            cache.get(&enabled_key).await.as_deref(),
            Some("enabled-url")
        );
        assert_eq!(cache.get(&unknown_key).await, None);
        assert_eq!(cache.get(&legacy_key).await, None);
        assert_eq!(cache.get_stats().await.max_capacity, 10);
    }

    #[tokio::test]
    async fn per_server_ttl_isolated() {
        let cache = DirectLinkCache::new_with_server_policies(
            policy(false, 0, 0),
            [
                ("short".to_string(), policy(true, 1, 10)),
                ("long".to_string(), policy(true, 60, 10)),
            ],
        );
        let short_key = key("short", "media", "default", "ua");
        let long_key = key("long", "media", "default", "ua");
        cache.set(short_key.clone(), "short-url".to_string()).await;
        cache.set(long_key.clone(), "long-url".to_string()).await;

        tokio::time::sleep(TokioDuration::from_millis(1_200)).await;
        assert_eq!(cache.get(&short_key).await, None);
        assert_eq!(cache.get(&long_key).await.as_deref(), Some("long-url"));
    }

    #[tokio::test]
    async fn per_server_capacity_isolated() {
        let cache = DirectLinkCache::new_with_server_policies(
            policy(false, 0, 0),
            [
                ("small".to_string(), policy(true, 60, 1)),
                ("large".to_string(), policy(true, 60, 3)),
            ],
        );
        for index in 0..3 {
            cache
                .set(
                    key("small", &format!("media{index}"), "default", "ua"),
                    format!("small-{index}"),
                )
                .await;
            cache
                .set(
                    key("large", &format!("media{index}"), "default", "ua"),
                    format!("large-{index}"),
                )
                .await;
        }
        for bucket in cache.all_buckets() {
            bucket.inner.run_pending_tasks().await;
        }

        assert_eq!(
            cache
                .search_entries(Some("small".to_string()), None)
                .await
                .len(),
            1
        );
        assert_eq!(
            cache
                .search_entries(Some("large".to_string()), None)
                .await
                .len(),
            3
        );
        let stats = cache.get_stats().await;
        assert_eq!(stats.capacity, 4);
        assert_eq!(stats.max_capacity, 4);
    }

    #[tokio::test]
    async fn server_cleanup_isolated_and_stats_aggregate() {
        let cache = DirectLinkCache::new_with_server_policies(
            policy(true, 60, 5),
            [
                ("server1".to_string(), policy(true, 60, 2)),
                ("server2".to_string(), policy(true, 60, 3)),
                ("disabled".to_string(), policy(false, 60, 50)),
            ],
        );
        let server1_key = key("server1", "media1", "default", "ua");
        let server2_key = key("server2", "media1", "default", "ua");
        let unknown_key = key("unknown", "media1", "default", "ua");
        cache.set(server1_key.clone(), "url1".to_string()).await;
        cache.set(server2_key.clone(), "url2".to_string()).await;
        cache.set(unknown_key.clone(), "url3".to_string()).await;

        assert_eq!(cache.get_stats().await.max_capacity, 10);
        assert_eq!(
            cache
                .clean_by_server_media(Some("server1".to_string()), None)
                .await,
            1
        );
        assert_eq!(cache.get(&server1_key).await, None);
        assert_eq!(cache.get(&server2_key).await.as_deref(), Some("url2"));
        assert_eq!(cache.get(&unknown_key).await.as_deref(), Some("url3"));
        let stats = cache.get_stats().await;
        assert_eq!(stats.capacity, 2);
        assert_eq!(stats.max_capacity, 10);
        assert_eq!(stats.hits, 2);
        assert_eq!(stats.misses, 1);
        assert_eq!(stats.hit_rate, 2.0 / 3.0);
    }

    #[tokio::test]
    async fn strategy_constructor_builds_real_server_buckets() {
        let cache = DirectLinkCache::new_with_strategy(
            true,
            60,
            1,
            vec!["server1".to_string(), "server2".to_string()],
        );
        cache
            .set(
                key("server1", "media1", "default", "ua"),
                "url1".to_string(),
            )
            .await;
        cache
            .set(
                key("server2", "media2", "default", "ua"),
                "url2".to_string(),
            )
            .await;
        assert_eq!(cache.get_stats().await.capacity, 2);
        // Global fallback plus two explicit buckets.
        assert_eq!(cache.get_stats().await.max_capacity, 3);
    }

    #[tokio::test]
    async fn safe_views_sort_filter_and_hide_secrets() {
        let cache = DirectLinkCache::new(true, 60, 100);
        cache
            .set(
                key("server2", "media2", "default", "secret:ua"),
                "https://example.test/path?token=secret".to_string(),
            )
            .await;
        cache
            .set(
                key("server1", "media1", "default", "ua"),
                "https://example.test/other".to_string(),
            )
            .await;

        let views = cache.list_entry_views(Some("SERVER")).await;
        assert_eq!(views.len(), 2);
        assert_eq!(views[0].server_id, "server1");
        assert_eq!(views[1].server_id, "server2");
        let json = serde_json::to_string(&views).unwrap();
        assert!(!json.contains("https://"));
        assert!(!json.contains("token="));
        assert!(!json.contains("secret:ua"));
    }

    #[tokio::test]
    async fn safe_views_preserve_legacy_sort_filters_and_legacy_entries() {
        let cache = DirectLinkCache::new_with_server_policies(
            policy(true, 60, 100),
            [
                ("server-a".to_string(), policy(true, 60, 100)),
                ("server-b".to_string(), policy(true, 60, 100)),
            ],
        );
        for (cache_key, value) in vec![
            ("legacy-key".to_string(), "legacy-url".to_string()),
            (
                key("server-a", "media-2", "source-1", "ua-a"),
                "url-a2".to_string(),
            ),
            (
                key("server-a", "media-1", "source-2", "ua-a"),
                "url-a1".to_string(),
            ),
            (
                key("server-b", "media-2", "source-2", "ua-b"),
                "url-b2".to_string(),
            ),
            (
                key("server-c", "media-3", "source-3", "ua-c"),
                "url-c3".to_string(),
            ),
        ] {
            cache.set(cache_key, value).await;
        }

        let duplicate_key = key("server-a", "media-0", "source-0", "ua-duplicate");
        cache
            .default_bucket
            .inner
            .insert(duplicate_key.clone(), "url-default".to_string())
            .await;
        cache
            .server_buckets
            .get("server-a")
            .unwrap()
            .inner
            .insert(duplicate_key, "url-server".to_string())
            .await;
        for bucket in cache.all_buckets() {
            bucket.inner.run_pending_tasks().await;
        }

        let expected_all = legacy_view_page(&cache, None, None, None, 0, usize::MAX).await;
        let actual_all = cache.list_entry_views(None).await;
        assert_eq!(actual_all, expected_all);
        assert!(actual_all.iter().any(|view| view.legacy));
        assert_eq!(
            actual_all
                .iter()
                .filter(|view| view.media_id == "media-0")
                .count(),
            1
        );

        let allowed_server_ids = ["server-a".to_string(), "server-b".to_string()]
            .into_iter()
            .collect::<HashSet<_>>();
        let expected_filtered = legacy_view_page(
            &cache,
            Some("MEDIA"),
            None,
            Some(&allowed_server_ids),
            0,
            20,
        )
        .await;
        let actual_filtered = cache
            .list_entry_views_page(Some("MEDIA"), None, Some(&allowed_server_ids), 0, 20)
            .await;
        assert_eq!(actual_filtered, expected_filtered);
        assert!(
            actual_filtered
                .iter()
                .all(|view| allowed_server_ids.contains(&view.server_id))
        );

        let expected_requested_empty =
            legacy_view_page(&cache, Some(""), Some(""), None, 0, usize::MAX).await;
        let actual_requested_empty = cache
            .list_entry_views_page(Some(""), Some(""), None, 0, usize::MAX)
            .await;
        assert_eq!(actual_requested_empty, expected_requested_empty);
        assert_eq!(actual_requested_empty.len(), 1);
        assert!(actual_requested_empty[0].legacy);

        let empty_allowed = HashSet::new();
        let expected_empty_allowed =
            legacy_view_page(&cache, None, None, Some(&empty_allowed), 0, usize::MAX).await;
        let actual_empty_allowed = cache
            .list_entry_views_page(None, None, Some(&empty_allowed), 0, usize::MAX)
            .await;
        assert_eq!(actual_empty_allowed, expected_empty_allowed);
        assert!(actual_empty_allowed.is_empty());

        let expected_page = legacy_view_page(&cache, None, None, None, 1, 2).await;
        let actual_page = cache.list_entry_views_page(None, None, None, 1, 2).await;
        assert_eq!(actual_page, expected_page);
    }

    #[tokio::test]
    async fn safe_views_apply_hard_cap_before_pagination_and_authorization() {
        let cache = DirectLinkCache::new(true, 60, (MAX_ENTRY_LIST + 2) as u64);
        for index in 0..=MAX_ENTRY_LIST {
            cache
                .set(
                    key(
                        "server-a",
                        &format!("media-{index:04}"),
                        "source",
                        &format!("ua-{index}"),
                    ),
                    format!("url-{index}"),
                )
                .await;
        }
        cache
            .set(
                key("server-z", "media-9999", "source", "ua-z"),
                "url-z".to_string(),
            )
            .await;
        for bucket in cache.all_buckets() {
            bucket.inner.run_pending_tasks().await;
        }

        let expected_all = legacy_view_page(&cache, None, None, None, 0, usize::MAX).await;
        let actual_all = cache.list_entry_views(None).await;
        assert_eq!(actual_all.len(), MAX_ENTRY_LIST);
        assert_eq!(actual_all, expected_all);
        assert_eq!(
            cache
                .list_entry_views_page(None, None, None, MAX_ENTRY_LIST - 1, 10)
                .await,
            actual_all[MAX_ENTRY_LIST - 1..].to_vec()
        );
        assert!(
            cache
                .list_entry_views_page(None, None, None, MAX_ENTRY_LIST, 1)
                .await
                .is_empty()
        );

        let allowed_server_ids = ["server-z".to_string()].into_iter().collect::<HashSet<_>>();
        let expected_authorized =
            legacy_view_page(&cache, None, None, Some(&allowed_server_ids), 0, 10).await;
        let actual_authorized = cache
            .list_entry_views_page(None, None, Some(&allowed_server_ids), 0, 10)
            .await;
        assert_eq!(actual_authorized, expected_authorized);
        assert_eq!(actual_authorized.len(), 1);
        assert_eq!(actual_authorized[0].server_id, "server-z");
        assert_eq!(
            cache
                .list_entry_views_page(None, Some("server-z"), None, 0, 10)
                .await,
            vec![actual_authorized[0].clone()]
        );
    }

    #[test]
    fn safe_view_sort_uses_raw_key_only_as_visible_tie_breaker() {
        let view = CacheEntryView {
            id: "same-id".to_string(),
            server_id: "server".to_string(),
            media_id: "media".to_string(),
            source_id: "source".to_string(),
            legacy: false,
        };
        let mut entries = vec![
            (view.clone(), "raw-z".to_string()),
            (view, "raw-a".to_string()),
        ];

        DirectLinkCache::sort_view_entries(&mut entries);

        assert_eq!(entries[0].1, "raw-a");
        assert_eq!(entries[1].1, "raw-z");

        let mut heap = BinaryHeap::new();
        for (view, raw_key) in entries {
            heap.push(RankedEntry { view, raw_key });
        }
        assert_eq!(heap.pop().unwrap().raw_key, "raw-z");
        assert_eq!(heap.pop().unwrap().raw_key, "raw-a");
    }

    #[tokio::test]
    async fn safe_views_top_k_matches_full_sort_across_buckets_and_pages() {
        let entries_per_server = MAX_ENTRY_LIST + 37;
        let cache = DirectLinkCache::new_with_server_policies(
            policy(true, 60, 5_000),
            ["server-a", "server-b", "server-z"]
                .map(|server| (server.to_string(), policy(true, 60, 5_000))),
        );
        // Reverse insertion and shared field tuples exercise every sort field,
        // including the opaque ID for keys differing only in their private UA.
        for server in ["server-z", "server-b", "server-a"] {
            for index in (0..entries_per_server).rev() {
                let raw_key = key(
                    server,
                    &format!("media-{:04}", index / 9),
                    &format!("source-{}", index % 3),
                    &format!("ua-secret-{index}"),
                );
                cache.set(raw_key.clone(), "private-url".to_string()).await;
                if index % 7 == 0 {
                    cache
                        .default_bucket
                        .inner
                        .insert(raw_key, "duplicate-url".to_string())
                        .await;
                }
            }
        }
        for raw_key in [
            "legacy-secret",
            ":media:source:ua",
            "server::source:ua",
            "a:b:c",
        ] {
            cache
                .set(raw_key.to_string(), "private-url".to_string())
                .await;
        }
        assert_eq!(cache.current_keys().await.len(), entries_per_server * 3 + 4);

        let allowed = HashSet::from(["server-z".to_string()]);
        let allowed_legacy = HashSet::from([String::new()]);
        let empty_allowed = HashSet::new();
        let sample_id =
            DirectLinkCache::opaque_id(&key("server-z", "media-0000", "source-0", "ua-secret-0"));
        for (search, requested, allowed) in [
            (None, None, None),
            (Some("  MEDIA-  "), None, None),
            (Some("SOURCE-1"), None, None),
            (None, None, Some(&allowed)),
            (None, Some("server-z"), Some(&allowed)),
            (None, Some("server-a"), Some(&allowed)),
            (None, Some(""), None),
            (None, None, Some(&allowed_legacy)),
            (None, None, Some(&empty_allowed)),
            (Some(sample_id.as_str()), None, None),
            (Some("ua-secret"), None, None),
            (Some("private-url"), None, None),
        ] {
            let expected =
                legacy_view_page(&cache, search, requested, allowed, 0, usize::MAX).await;
            let actual = cache
                .list_entry_views_page(search, requested, allowed, 0, usize::MAX)
                .await;
            assert_eq!(
                actual, expected,
                "search={search:?}, requested={requested:?}"
            );
            assert!(actual.len() <= MAX_ENTRY_LIST);
            for (start, limit) in [
                (0, 0),
                (0, 31),
                (31, 19),
                (MAX_ENTRY_LIST - 13, 40),
                (MAX_ENTRY_LIST, 10),
                (usize::MAX, usize::MAX),
            ] {
                let page = cache
                    .list_entry_views_page(search, requested, allowed, start, limit)
                    .await;
                let expected_page = expected
                    .iter()
                    .skip(start)
                    .take(limit)
                    .cloned()
                    .collect::<Vec<_>>();
                assert_eq!(page, expected_page, "start={start}, limit={limit}");
            }
        }
        let views = cache.list_entry_views(None).await;
        assert_eq!(views.len(), MAX_ENTRY_LIST);
        let serialized = serde_json::to_string(&views).unwrap();
        assert!(!serialized.contains("ua-secret"));
        assert!(!serialized.contains("private-url"));
    }

    #[test]
    fn parse_key_requires_four_segments() {
        assert!(DirectLinkCache::parse_key("a:b:c:d").is_some());
        assert_eq!(DirectLinkCache::parse_key("a:b:c:d").unwrap().rest, "d");
        assert!(DirectLinkCache::parse_key("a:b:c").is_none());
        assert!(DirectLinkCache::parse_key("").is_none());
        assert!(DirectLinkCache::parse_key(":::").is_none());
    }
}
