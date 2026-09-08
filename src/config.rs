use anyhow::{Context, Result};
use serde::Deserialize;
use std::{collections::HashMap, fs, net::SocketAddr, path::Path, path::PathBuf};

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    pub listen: SocketAddr,
    pub websocket_path: String,
    pub websocket_subprotocol: String,
    pub route_without_affiliations: bool,
    pub fallback_broadcast_when_no_affiliations: bool,
    pub allow_multiple_calls_per_group: bool,
    pub higher_priority_number_wins: bool,
    pub preempt_cause: u8,
    pub auth: AuthConfig,
    pub tls: TlsConfig,
    pub telemetry: TelemetryConfig,
    pub control: ControlConfig,
    pub dashboard: DashboardConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct TlsConfig {
    pub enabled: bool,
    pub cert_path: PathBuf,
    pub key_path: PathBuf,
}

impl Default for TlsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            cert_path: PathBuf::from("cert.pem"),
            key_path: PathBuf::from("key.pem"),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct AuthConfig {
    pub enabled: bool,
    pub realm: String,
    pub users: HashMap<String, String>,
    pub session_ttl_seconds: u64,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            realm: "brew-server".into(),
            users: HashMap::new(),
            session_ttl_seconds: 300,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct TelemetryConfig {
    pub enabled: bool,
    pub listen: SocketAddr,
    /// HTTP Basic Auth username -> password. Empty means no auth required.
    pub users: HashMap<String, String>,
    pub tls: TlsConfig,
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            listen: "0.0.0.0:9001".parse().unwrap(),
            users: HashMap::new(),
            tls: TlsConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ControlConfig {
    pub enabled: bool,
    pub listen: SocketAddr,
    /// HTTP Basic Auth username -> password. Empty means no auth required.
    pub users: HashMap<String, String>,
    pub tls: TlsConfig,
}

impl Default for ControlConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            listen: "0.0.0.0:9002".parse().unwrap(),
            users: HashMap::new(),
            tls: TlsConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct DashboardConfig {
    pub enabled: bool,
    pub listen: SocketAddr,
    /// HTTP Basic Auth username -> password. Empty means no auth required.
    pub users: HashMap<String, String>,
    pub realm: String,
    pub tls: TlsConfig,
}

impl Default for DashboardConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            listen: "0.0.0.0:9003".parse().unwrap(),
            users: HashMap::new(),
            realm: "brew-server-dashboard".into(),
            tls: TlsConfig::default(),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen: "0.0.0.0:9000".parse().unwrap(),
            websocket_path: "/brew".into(),
            websocket_subprotocol: "brew".into(),
            route_without_affiliations: false,
            fallback_broadcast_when_no_affiliations: true,
            allow_multiple_calls_per_group: false,
            higher_priority_number_wins: true,
            preempt_cause: 1,
            auth: AuthConfig::default(),
            tls: TlsConfig::default(),
            telemetry: TelemetryConfig::default(),
            control: ControlConfig::default(),
            dashboard: DashboardConfig::default(),
        }
    }
}

impl Config {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = fs::read_to_string(path)
            .with_context(|| format!("reading {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }
}
