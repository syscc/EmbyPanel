use std::net::IpAddr;

use ipnet::IpNet;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::{
    error::{AppError, AppResult},
    url_mapping::{self, UrlMappingRule},
};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Config {
    pub emby_host: String,
    pub emby_api_key: String,
    #[serde(default)]
    pub servers: Vec<EmbyServerConfig>,
    pub openlist_addr: Option<String>,
    pub openlist_token: Option<String>,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_cache_ttl_seconds")]
    pub cache_ttl_seconds: u64,
    #[serde(default = "default_cache_max_capacity")]
    pub cache_max_capacity: u64,
    #[serde(default = "default_cache_enabled")]
    pub cache_enabled: bool,
    #[serde(default = "default_cache_domain_filter_mode")]
    pub cache_domain_filter_mode: String,
    #[serde(default)]
    pub cache_domain_whitelist: String,
    #[serde(default, skip_serializing)]
    pub cache_domain_blacklist: String,
    #[serde(default)]
    pub enable_internal_redirect: bool,
    #[serde(default = "default_internal_redirect_timeout_seconds")]
    pub internal_redirect_timeout_seconds: u64,
    #[serde(default)]
    pub strm_url_mappings: String,
    #[serde(default = "default_strm_url_mapping_enabled")]
    pub strm_url_mapping_enabled: bool,
    #[serde(default = "default_connectivity_check_enabled")]
    pub connectivity_check_enabled: bool,
    #[serde(default = "default_connectivity_check_interval_seconds")]
    pub connectivity_check_interval_seconds: u64,
    #[serde(default = "default_connectivity_check_timeout_seconds")]
    pub connectivity_check_timeout_seconds: u64,
    #[serde(default = "default_connectivity_auto_restart_seconds")]
    pub connectivity_auto_restart_seconds: u64,
    #[serde(skip)]
    pub strm_url_mapping_rules: Vec<UrlMappingRule>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct EmbyServerConfig {
    pub id: String,
    pub name: String,
    pub emby_host: String,
    pub emby_api_key: String,
    pub port: u16,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub block_web_ui: bool,
    #[serde(default = "default_real_ip_mode")]
    pub real_ip_mode: String,
    #[serde(default)]
    pub real_ip_header: String,
    #[serde(default)]
    pub trusted_proxy_cidrs: String,
    #[serde(skip)]
    pub(crate) trusted_proxy_networks: Vec<IpNet>,
}

impl Config {
    pub fn default_runtime() -> Self {
        Self {
            emby_host: "http://localhost:8096".to_string(),
            emby_api_key: String::new(),
            servers: Vec::new(),
            openlist_addr: None,
            openlist_token: None,
            port: default_port(),
            cache_ttl_seconds: default_cache_ttl_seconds(),
            cache_max_capacity: default_cache_max_capacity(),
            cache_enabled: default_cache_enabled(),
            cache_domain_filter_mode: default_cache_domain_filter_mode(),
            cache_domain_whitelist: String::new(),
            cache_domain_blacklist: String::new(),
            enable_internal_redirect: false,
            internal_redirect_timeout_seconds: default_internal_redirect_timeout_seconds(),
            strm_url_mappings: String::new(),
            strm_url_mapping_enabled: default_strm_url_mapping_enabled(),
            connectivity_check_enabled: default_connectivity_check_enabled(),
            connectivity_check_interval_seconds: default_connectivity_check_interval_seconds(),
            connectivity_check_timeout_seconds: default_connectivity_check_timeout_seconds(),
            connectivity_auto_restart_seconds: default_connectivity_auto_restart_seconds(),
            strm_url_mapping_rules: Vec::new(),
        }
    }

    pub fn validate_for_storage(&mut self) -> AppResult<()> {
        self.normalize_and_validate()
    }

    pub fn proxy_configs(&self) -> Vec<Self> {
        self.servers
            .iter()
            .filter(|server| server.enabled && !server.emby_api_key.trim().is_empty())
            .map(|server| self.for_server(server))
            .collect()
    }

    pub fn proxy_config_for_server(&self, server_id: Option<&str>) -> Self {
        let server = server_id
            .and_then(|id| self.servers.iter().find(|server| server.id == id))
            .or_else(|| self.servers.iter().find(|server| server.enabled))
            .or_else(|| self.servers.first());
        server
            .map(|server| self.for_server(server))
            .unwrap_or_else(|| self.clone())
    }

    pub fn emby_url(&self, path_and_query: &str) -> AppResult<Url> {
        join_base_and_path(&self.emby_host, path_and_query)
    }

    pub fn openlist_url(&self, path: &str) -> AppResult<Url> {
        let Some(openlist_addr) = self.openlist_addr.as_deref() else {
            return Err(AppError::Config(
                "openlist_addr is required for OpenList /d path resolving".to_string(),
            ));
        };
        join_base_and_path(openlist_addr, path)
    }

    fn normalize_and_validate(&mut self) -> AppResult<()> {
        self.normalize_servers()?;
        self.openlist_addr = self
            .openlist_addr
            .as_deref()
            .map(|value| trim_base_url("openlist_addr", value))
            .transpose()?;
        self.openlist_token = self
            .openlist_token
            .as_deref()
            .map(|value| required("openlist_token", value))
            .transpose()?;
        if self.openlist_addr.is_some() != self.openlist_token.is_some() {
            return Err(AppError::Config(
                "openlist_addr and openlist_token must be configured together".to_string(),
            ));
        }
        if self.cache_max_capacity == 0 {
            return Err(AppError::Config(
                "cache_max_capacity must be positive".to_string(),
            ));
        }
        self.cache_domain_whitelist = normalize_multiline(&self.cache_domain_whitelist);
        self.cache_domain_blacklist = normalize_multiline(&self.cache_domain_blacklist);
        self.cache_domain_filter_mode =
            normalize_cache_domain_filter_mode(&self.cache_domain_filter_mode)?;
        if self.cache_domain_filter_mode == "blacklist" && self.cache_domain_whitelist.is_empty() {
            self.cache_domain_whitelist = self.cache_domain_blacklist.clone();
        }
        self.cache_domain_blacklist = String::new();
        if self.internal_redirect_timeout_seconds == 0 {
            return Err(AppError::Config(
                "internal_redirect_timeout_seconds must be positive".to_string(),
            ));
        }
        self.connectivity_check_interval_seconds =
            self.connectivity_check_interval_seconds.clamp(10, 3600);
        self.connectivity_check_timeout_seconds =
            self.connectivity_check_timeout_seconds.clamp(1, 60);
        self.connectivity_auto_restart_seconds = self.connectivity_auto_restart_seconds.min(86400);
        self.strm_url_mapping_rules = url_mapping::parse_rules(&self.strm_url_mappings)?;
        Ok(())
    }

    fn normalize_servers(&mut self) -> AppResult<()> {
        if self.servers.is_empty() {
            if !self.emby_host.trim().is_empty() || !self.emby_api_key.trim().is_empty() {
                self.emby_host = trim_base_url("emby_host", &self.emby_host)?;
                self.emby_api_key = required("emby_api_key", &self.emby_api_key)?;
                validate_port(self.port)?;
                self.servers.push(EmbyServerConfig {
                    id: "default".to_string(),
                    name: "默认服务器".to_string(),
                    emby_host: self.emby_host.clone(),
                    emby_api_key: self.emby_api_key.clone(),
                    port: self.port,
                    enabled: true,
                    block_web_ui: false,
                    real_ip_mode: default_real_ip_mode(),
                    real_ip_header: String::new(),
                    trusted_proxy_cidrs: String::new(),
                    trusted_proxy_networks: Vec::new(),
                });
                return Ok(());
            }
            validate_port(self.port)?;
            self.emby_host = String::new();
            self.emby_api_key = String::new();
            return Ok(());
        }

        let mut ids = std::collections::HashSet::new();
        let mut ports = std::collections::HashSet::new();
        for (index, server) in self.servers.iter_mut().enumerate() {
            if server.id.trim().is_empty() {
                server.id = format!("server-{}", index + 1);
            } else {
                server.id = server.id.trim().to_string();
            }
            if !ids.insert(server.id.clone()) {
                return Err(AppError::Config(format!(
                    "server id {} is configured more than once",
                    server.id
                )));
            }
            if server.name.trim().is_empty() {
                server.name = format!("服务器 {}", index + 1);
            } else {
                server.name = server.name.trim().to_string();
            }
            server.emby_host = trim_base_url("server.emby_host", &server.emby_host)?;
            server.emby_api_key = required("server.emby_api_key", &server.emby_api_key)?;
            validate_port(server.port)?;
            server.real_ip_mode = normalize_real_ip_mode(&server.real_ip_mode)?;
            server.real_ip_header = normalize_header_list(&server.real_ip_header);
            let (trusted_proxy_cidrs, trusted_proxy_networks) =
                normalize_trusted_proxy_cidrs(&server.trusted_proxy_cidrs)?;
            server.trusted_proxy_cidrs = trusted_proxy_cidrs;
            server.trusted_proxy_networks = trusted_proxy_networks;
            if server.real_ip_mode == "header" && server.real_ip_header.is_empty() {
                return Err(AppError::Config(
                    "server.real_ip_header cannot be empty when real_ip_mode is header".to_string(),
                ));
            }
            if !ports.insert(server.port) {
                return Err(AppError::Config(format!(
                    "proxy port {} is configured more than once",
                    server.port
                )));
            }
        }

        let Some(primary) = self
            .servers
            .iter()
            .find(|server| server.enabled)
            .or_else(|| self.servers.first())
        else {
            return Err(AppError::Config(
                "at least one Emby server is required".to_string(),
            ));
        };
        self.emby_host = primary.emby_host.clone();
        self.emby_api_key = primary.emby_api_key.clone();
        self.port = primary.port;
        Ok(())
    }

    fn for_server(&self, server: &EmbyServerConfig) -> Self {
        let mut config = self.clone();
        config.emby_host = server.emby_host.clone();
        config.emby_api_key = server.emby_api_key.clone();
        config.port = server.port;
        config.servers = vec![server.clone()];
        config
    }

    pub fn for_server_for_validation(&self, server: &EmbyServerConfig) -> Self {
        self.for_server(server)
    }
}

impl EmbyServerConfig {
    pub(crate) fn is_trusted_proxy(&self, peer_ip: IpAddr) -> bool {
        let peer_ip = match peer_ip {
            IpAddr::V6(ip) => ip
                .to_ipv4_mapped()
                .map(IpAddr::V4)
                .unwrap_or(IpAddr::V6(ip)),
            peer_ip => peer_ip,
        };
        self.trusted_proxy_networks
            .iter()
            .any(|network| network.contains(&peer_ip))
    }
}

fn default_port() -> u16 {
    8096
}

fn default_enabled() -> bool {
    true
}

fn default_real_ip_mode() -> String {
    "auto".to_string()
}

fn default_cache_ttl_seconds() -> u64 {
    180
}

fn default_cache_max_capacity() -> u64 {
    10_000
}

fn default_cache_enabled() -> bool {
    true
}

fn default_cache_domain_filter_mode() -> String {
    "off".to_string()
}

fn default_internal_redirect_timeout_seconds() -> u64 {
    15
}

fn default_strm_url_mapping_enabled() -> bool {
    true
}

fn default_connectivity_check_enabled() -> bool {
    true
}

fn default_connectivity_check_interval_seconds() -> u64 {
    60
}

fn default_connectivity_check_timeout_seconds() -> u64 {
    5
}

fn default_connectivity_auto_restart_seconds() -> u64 {
    180
}

fn required(name: &str, value: &str) -> AppResult<String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(AppError::Config(format!("{name} cannot be empty")));
    }
    Ok(value.to_string())
}

fn trim_base_url(name: &str, value: &str) -> AppResult<String> {
    let value = required(name, value)?.trim_end_matches('/').to_string();
    Url::parse(&value)
        .map_err(|err| AppError::Config(format!("{name} must be a valid URL: {err}")))?;
    Ok(value)
}

fn validate_port(port: u16) -> AppResult<()> {
    if port == 0 {
        return Err(AppError::Config(
            "port must be between 1 and 65535".to_string(),
        ));
    }
    if port == 8090 {
        return Err(AppError::Config(
            "port 8090 is reserved for the management API".to_string(),
        ));
    }
    Ok(())
}

fn normalize_multiline(value: &str) -> String {
    value
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn normalize_header_list(value: &str) -> String {
    value
        .split([',', '\n'])
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn normalize_trusted_proxy_cidrs(value: &str) -> AppResult<(String, Vec<IpNet>)> {
    let mut networks = Vec::new();
    for item in value
        .split([',', '\n'])
        .map(str::trim)
        .filter(|item| !item.is_empty())
    {
        let network = match item.parse::<IpNet>() {
            Ok(network) => network.trunc(),
            Err(cidr_error) => match item.parse::<IpAddr>() {
                Ok(ip) => IpNet::from(ip),
                Err(_) => {
                    return Err(AppError::Config(format!(
                        "server.trusted_proxy_cidrs contains invalid IP or CIDR {item}: {cidr_error}"
                    )));
                }
            },
        };
        if !networks.contains(&network) {
            networks.push(network);
        }
    }
    let normalized = networks
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    Ok((normalized, networks))
}

fn normalize_real_ip_mode(value: &str) -> AppResult<String> {
    let mode = value.trim().to_ascii_lowercase();
    match mode.as_str() {
        "" | "auto" | "system" => Ok("auto".to_string()),
        "header" | "single_header" => Ok("header".to_string()),
        "header_list" | "headers" | "list" => Ok("header_list".to_string()),
        "xff_last" | "x-forwarded-for-last" => Ok("xff_last".to_string()),
        "xff_second_last" | "x-forwarded-for-second-last" => Ok("xff_second_last".to_string()),
        "xff_third_last" | "x-forwarded-for-third-last" => Ok("xff_third_last".to_string()),
        _ => Err(AppError::Config(format!(
            "real_ip_mode must be auto, header, header_list, xff_last, xff_second_last, or xff_third_last: {value}"
        ))),
    }
}

fn normalize_cache_domain_filter_mode(value: &str) -> AppResult<String> {
    let mode = value.trim().to_ascii_lowercase();
    match mode.as_str() {
        "" | "off" | "none" => Ok("off".to_string()),
        "whitelist" | "white" | "allow" => Ok("whitelist".to_string()),
        "blacklist" | "black" | "deny" => Ok("blacklist".to_string()),
        _ => Err(AppError::Config(format!(
            "cache_domain_filter_mode must be off, whitelist, or blacklist: {value}"
        ))),
    }
}

fn join_base_and_path(base: &str, path_and_query: &str) -> AppResult<Url> {
    let mut url = Url::parse(base)?;
    url.set_path(path_and_query.split('?').next().unwrap_or("/"));
    url.set_query(path_and_query.split_once('?').map(|(_, query)| query));
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_allows_zero_cache_ttl_to_disable_cache() {
        let mut config = Config {
            emby_host: "http://localhost:8096".to_string(),
            emby_api_key: "key".to_string(),
            servers: Vec::new(),
            openlist_addr: Some("http://localhost:5244".to_string()),
            openlist_token: Some("token".to_string()),
            port: 18096,
            cache_ttl_seconds: 0,
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
        };

        assert!(config.normalize_and_validate().is_ok());
    }

    #[test]
    fn config_requires_positive_cache_max_capacity() {
        let mut config = Config {
            emby_host: "http://localhost:8096".to_string(),
            emby_api_key: "key".to_string(),
            servers: Vec::new(),
            openlist_addr: None,
            openlist_token: None,
            port: 18096,
            cache_ttl_seconds: 180,
            cache_max_capacity: 0,
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
        };

        assert!(config.normalize_and_validate().is_err());
    }

    #[test]
    fn config_trims_base_urls() {
        let mut config = Config {
            emby_host: "http://localhost:8096/".to_string(),
            emby_api_key: "key".to_string(),
            servers: Vec::new(),
            openlist_addr: Some("http://localhost:5244/".to_string()),
            openlist_token: Some("token".to_string()),
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
        };

        config.normalize_and_validate().unwrap();

        assert_eq!(config.emby_host, "http://localhost:8096");
        assert_eq!(
            config.openlist_addr.as_deref(),
            Some("http://localhost:5244")
        );
    }

    #[test]
    fn config_allows_direct_url_mode_without_openlist() {
        let mut config = Config {
            emby_host: "http://localhost:8096".to_string(),
            emby_api_key: "key".to_string(),
            servers: Vec::new(),
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
        };

        assert!(config.normalize_and_validate().is_ok());
    }

    #[test]
    fn config_allows_empty_server_list_when_top_level_emby_is_empty() {
        let mut config = Config {
            emby_host: String::new(),
            emby_api_key: String::new(),
            servers: Vec::new(),
            openlist_addr: None,
            openlist_token: None,
            port: 8096,
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
        };

        config.normalize_and_validate().unwrap();

        assert!(config.servers.is_empty());
        assert!(config.emby_host.is_empty());
        assert!(config.emby_api_key.is_empty());
    }

    #[test]
    fn config_requires_openlist_pair_when_partially_configured() {
        let mut config = Config {
            emby_host: "http://localhost:8096".to_string(),
            emby_api_key: "key".to_string(),
            servers: Vec::new(),
            openlist_addr: Some("http://localhost:5244".to_string()),
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
        };

        assert!(config.normalize_and_validate().is_err());
    }

    #[test]
    fn config_requires_positive_internal_redirect_timeout() {
        let mut config = Config {
            emby_host: "http://localhost:8096".to_string(),
            emby_api_key: "key".to_string(),
            servers: Vec::new(),
            openlist_addr: None,
            openlist_token: None,
            port: 18096,
            cache_ttl_seconds: 180,
            cache_max_capacity: 10_000,
            cache_enabled: true,
            cache_domain_filter_mode: "off".to_string(),
            cache_domain_whitelist: String::new(),
            cache_domain_blacklist: String::new(),
            enable_internal_redirect: true,
            internal_redirect_timeout_seconds: 0,
            strm_url_mappings: String::new(),
            strm_url_mapping_enabled: true,
            connectivity_check_enabled: true,
            connectivity_check_interval_seconds: 60,
            connectivity_check_timeout_seconds: 5,
            connectivity_auto_restart_seconds: 180,
            strm_url_mapping_rules: Vec::new(),
        };

        assert!(config.normalize_and_validate().is_err());
    }

    #[test]
    fn config_parses_strm_url_mapping_rules() {
        let mut config = Config {
            emby_host: "http://localhost:8096".to_string(),
            emby_api_key: "key".to_string(),
            servers: Vec::new(),
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
            strm_url_mappings: "https://openlist.example.test => http://localhost:5244".to_string(),
            strm_url_mapping_enabled: true,
            connectivity_check_enabled: true,
            connectivity_check_interval_seconds: 60,
            connectivity_check_timeout_seconds: 5,
            connectivity_auto_restart_seconds: 180,
            strm_url_mapping_rules: Vec::new(),
        };

        config.normalize_and_validate().unwrap();

        assert_eq!(config.strm_url_mapping_rules.len(), 1);
    }

    #[test]
    fn config_defaults_feature_switches_to_enabled() {
        let mut value = serde_json::to_value(Config::default_runtime()).unwrap();
        let object = value.as_object_mut().unwrap();
        object.remove("cache_enabled");
        object.remove("strm_url_mapping_enabled");

        let config: Config = serde_json::from_value(value).unwrap();

        assert!(config.cache_enabled);
        assert!(config.strm_url_mapping_enabled);
    }

    #[test]
    fn trusted_proxy_cidrs_are_validated_and_normalized() {
        let (normalized, networks) =
            normalize_trusted_proxy_cidrs(" 192.0.2.9, 10.0.0.42/8\n2001:db8::7/32\n192.0.2.9 ")
                .unwrap();

        assert_eq!(normalized, "192.0.2.9/32\n10.0.0.0/8\n2001:db8::/32");
        assert_eq!(networks.len(), 3);
        assert!(networks[1].contains(&"10.20.30.40".parse::<IpAddr>().unwrap()));
        assert!(normalize_trusted_proxy_cidrs("10.0.0.0/99").is_err());
        assert!(normalize_trusted_proxy_cidrs("not-an-address").is_err());
    }

    #[test]
    fn server_config_defaults_web_ui_block_to_disabled() {
        let server: EmbyServerConfig = toml::from_str(
            r#"
id = "server-1"
name = "Server 1"
emby_host = "http://emby.test:8096"
emby_api_key = "key"
port = 18096
"#,
        )
        .unwrap();

        assert!(!server.block_web_ui);
    }
}
