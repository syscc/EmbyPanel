use std::time::Duration;

use moka::future::Cache;

#[derive(Clone)]
pub struct DirectLinkCache {
    enabled: bool,
    inner: Cache<String, String>,
}

impl DirectLinkCache {
    pub fn new(enabled: bool, ttl_seconds: u64, max_capacity: u64) -> Self {
        Self {
            enabled: enabled && ttl_seconds > 0,
            inner: Cache::builder()
                .time_to_live(Duration::from_secs(ttl_seconds.max(1)))
                .max_capacity(max_capacity)
                .build(),
        }
    }

    pub async fn get(&self, key: &str) -> Option<String> {
        if !self.enabled {
            return None;
        }
        self.inner.get(key).await
    }

    pub async fn set(&self, key: String, url: String) {
        if !self.enabled {
            return;
        }
        self.inner.insert(key, url).await;
    }
}
