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

    pub fn display_text(&self) -> String {
        let mut parts = Vec::new();
        for value in [
            &self.country_name,
            &self.region_name,
            &self.city_name,
            &self.district_name,
            &self.isp_domain,
        ] {
            let value = value.trim();
            if !value.is_empty() && !is_noise_location_part(value) && !parts.contains(&value) {
                parts.push(value);
            }
        }
        if parts.iter().any(|part| is_private_location_part(part)) {
            return "内网 IP".to_string();
        }
        parts.join(" ")
    }
}

fn is_noise_location_part(value: &str) -> bool {
    matches!(value, "IANA" | "Private-Use")
}

fn is_private_location_part(value: &str) -> bool {
    value.contains("局域网") || value.eq_ignore_ascii_case("private-use")
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
        if ip.is_empty() || ip == "--" {
            return None;
        }
        let Ok(parsed_ip) = ip.parse::<IpAddr>() else {
            return None;
        };
        if is_private_or_local_ip(&parsed_ip) {
            return Some(IpLocation {
                country_name: "内网 IP".to_string(),
                region_name: String::new(),
                city_name: String::new(),
                district_name: String::new(),
                isp_domain: String::new(),
            });
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

fn is_private_or_local_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_unspecified()
                || ip.octets()[0] == 100 && (64..=127).contains(&ip.octets()[1])
        }
        IpAddr::V6(ip) => {
            ip.is_loopback()
                || ip.is_unspecified()
                || (ip.segments()[0] & 0xfe00) == 0xfc00
                || (ip.segments()[0] & 0xffc0) == 0xfe80
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_location_display_is_clean() {
        let location = IpLocation {
            country_name: "IANA".to_string(),
            region_name: "局域网IP".to_string(),
            city_name: "Private-Use".to_string(),
            district_name: String::new(),
            isp_domain: String::new(),
        };

        assert_eq!(location.display_text(), "内网 IP");
    }

    #[test]
    fn detects_private_and_local_ips() {
        assert!(is_private_or_local_ip(&"10.0.0.3".parse().unwrap()));
        assert!(is_private_or_local_ip(&"192.168.1.2".parse().unwrap()));
        assert!(is_private_or_local_ip(&"172.16.0.1".parse().unwrap()));
        assert!(is_private_or_local_ip(&"127.0.0.1".parse().unwrap()));
        assert!(is_private_or_local_ip(&"fc00::1".parse().unwrap()));
        assert!(!is_private_or_local_ip(&"192.0.2.1".parse().unwrap()));
    }
}
