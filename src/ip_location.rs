use std::{
    env,
    fmt::Write as _,
    net::IpAddr,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use tokio::sync::RwLock;

use crate::{
    db,
    error::{AppError, AppResult},
};

const IPDB_FILE_NAME: &str = "qqwry.ipdb";
const DEFAULT_IPDB_DOWNLOAD_URL: &str =
    "https://raw.githubusercontent.com/nmgliangwei/qqwry.ipdb/main/qqwry.ipdb";
const DEFAULT_IPDB_MAX_BYTES: u64 = 64 * 1024 * 1024;
const IPDB_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(120);
const IPDB_URL_ENV: &str = "EMBYPANEL_IPDB_URL";
const IPDB_MAX_BYTES_ENV: &str = "EMBYPANEL_IPDB_MAX_BYTES";
const IPDB_SHA256_ENV: &str = "EMBYPANEL_IPDB_SHA256";

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
    let url = configured_ipdb_url()?;
    let max_bytes = configured_ipdb_max_bytes()?;
    let expected_sha256 = configured_ipdb_sha256()?;
    let mut response = client
        .get(url)
        .timeout(IPDB_DOWNLOAD_TIMEOUT)
        .send()
        .await?
        .error_for_status()?;
    if response.status().is_redirection() {
        return Err(AppError::BadGateway(
            "IP database download returned an unexpected redirect".to_string(),
        ));
    }
    if response
        .content_length()
        .is_some_and(|content_length| content_length > max_bytes)
    {
        return Err(AppError::BadGateway(format!(
            "IP database exceeds the configured {max_bytes} byte limit"
        )));
    }

    let temp_path = path.with_extension("ipdb.tmp");
    let download_result = async {
        let mut file = tokio::fs::File::create(&temp_path).await?;
        let mut hasher = Sha256::new();
        let mut downloaded = 0_u64;
        while let Some(chunk) = response.chunk().await? {
            downloaded = downloaded
                .checked_add(chunk.len() as u64)
                .ok_or_else(|| AppError::BadGateway("IP database is too large".to_string()))?;
            if downloaded > max_bytes {
                return Err(AppError::BadGateway(format!(
                    "IP database exceeds the configured {max_bytes} byte limit"
                )));
            }
            file.write_all(&chunk).await?;
            hasher.update(&chunk);
        }
        file.flush().await?;
        if downloaded == 0 {
            return Err(AppError::BadGateway(
                "downloaded IP database is empty".to_string(),
            ));
        }
        Ok((downloaded, sha256_hex(&hasher.finalize())))
    }
    .await;
    let (downloaded, actual_sha256) = match download_result {
        Ok(result) => result,
        Err(err) => {
            let _ = tokio::fs::remove_file(&temp_path).await;
            return Err(err);
        }
    };
    if expected_sha256
        .as_deref()
        .is_some_and(|expected| expected != actual_sha256)
    {
        let _ = tokio::fs::remove_file(&temp_path).await;
        return Err(AppError::BadGateway(
            "downloaded IP database SHA-256 does not match".to_string(),
        ));
    }
    if let Err(err) = ipdb::Reader::open_file(&temp_path) {
        let _ = tokio::fs::remove_file(&temp_path).await;
        return Err(AppError::BadGateway(format!(
            "downloaded IP database is invalid: {err}"
        )));
    }
    if let Err(err) = tokio::fs::rename(&temp_path, path).await {
        let _ = tokio::fs::remove_file(&temp_path).await;
        return Err(err.into());
    }
    tracing::info!(path = %path.display(), bytes = downloaded, "IP database downloaded");
    Ok(())
}

fn configured_ipdb_url() -> AppResult<reqwest::Url> {
    let value = env::var(IPDB_URL_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_IPDB_DOWNLOAD_URL.to_string());
    let url = reqwest::Url::parse(value.trim())?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(AppError::Config(format!(
            "{IPDB_URL_ENV} must use http or https"
        )));
    }
    Ok(url)
}

fn configured_ipdb_max_bytes() -> AppResult<u64> {
    let Some(value) = env::var(IPDB_MAX_BYTES_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
    else {
        return Ok(DEFAULT_IPDB_MAX_BYTES);
    };
    value
        .trim()
        .parse::<u64>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| AppError::Config(format!("{IPDB_MAX_BYTES_ENV} must be a positive integer")))
}

fn configured_ipdb_sha256() -> AppResult<Option<String>> {
    let Some(value) = env::var(IPDB_SHA256_ENV)
        .ok()
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(AppError::Config(format!(
            "{IPDB_SHA256_ENV} must contain 64 hexadecimal characters"
        )));
    }
    Ok(Some(value))
}

fn sha256_hex(digest: &[u8]) -> String {
    let mut value = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(&mut value, "{byte:02x}");
    }
    value
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
