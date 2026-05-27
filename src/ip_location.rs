use std::{
    net::IpAddr,
    path::{Path, PathBuf},
    sync::Arc,
};

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::{
    db,
    error::{AppError, AppResult},
};

const IPDB_FILE_NAME: &str = "qqwry.ipdb";
const IPDB_DOWNLOAD_URL: &str = "https://cdn.1008.site/gh/nmgliangwei/qqwry.ipdb@main/qqwry.ipdb";

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct IpLocation {
    pub country_name: String,
    pub region_name: String,
    pub city_name: String,
    pub district_name: String,
    pub isp_domain: String,
}

impl IpLocation {
    fn is_empty(&self) -> bool {
        self.country_name.is_empty()
            && self.region_name.is_empty()
            && self.city_name.is_empty()
            && self.district_name.is_empty()
            && self.isp_domain.is_empty()
    }
}

#[derive(Clone)]
pub struct IpLocationStore {
    inner: Arc<RwLock<Option<Arc<ipdb::Reader>>>>,
    path: PathBuf,
}

impl IpLocationStore {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(None)),
            path: db::data_dir().join(IPDB_FILE_NAME),
        }
    }

    pub async fn initialize(&self, client: &reqwest::Client) -> AppResult<()> {
        ensure_ipdb_file(client, &self.path).await?;
        match ipdb::Reader::open_file(&self.path) {
            Ok(reader) => {
                *self.inner.write().await = Some(Arc::new(reader));
                tracing::info!(path = %self.path.display(), "IP database loaded");
                Ok(())
            }
            Err(err) => Err(AppError::Internal(format!(
                "failed to load IP database {}: {err}",
                self.path.display()
            ))),
        }
    }

    pub async fn lookup(&self, ip: &str) -> Option<IpLocation> {
        let ip = ip.trim();
        if ip.is_empty() || ip == "--" || ip.parse::<IpAddr>().is_err() {
            return None;
        }
        let reader = self.inner.read().await.as_ref()?.clone();
        let map = reader.find_map(ip, "CN").ok()?;
        let location = IpLocation {
            country_name: map.get("country_name").copied().unwrap_or("").to_string(),
            region_name: map.get("region_name").copied().unwrap_or("").to_string(),
            city_name: map.get("city_name").copied().unwrap_or("").to_string(),
            district_name: map.get("district_name").copied().unwrap_or("").to_string(),
            isp_domain: map.get("isp_domain").copied().unwrap_or("").to_string(),
        };
        (!location.is_empty()).then_some(location)
    }
}

async fn ensure_ipdb_file(client: &reqwest::Client, path: &Path) -> AppResult<()> {
    if path.exists()
        && path
            .metadata()
            .map(|metadata| metadata.len() > 0)
            .unwrap_or(false)
    {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let response = client
        .get(IPDB_DOWNLOAD_URL)
        .send()
        .await?
        .error_for_status()?;
    let bytes = response.bytes().await?;
    if bytes.is_empty() {
        return Err(AppError::BadGateway(
            "downloaded IP database is empty".to_string(),
        ));
    }
    let temp_path = path.with_extension("ipdb.tmp");
    tokio::fs::write(&temp_path, &bytes).await?;
    tokio::fs::rename(&temp_path, path).await?;
    tracing::info!(path = %path.display(), bytes = bytes.len(), "IP database downloaded");
    Ok(())
}
