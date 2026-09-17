use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, fs, net::SocketAddr, path::Path, path::PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    pub storage: StorageConfig,
    pub sip: SipConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct StorageConfig {
    /// When enabled, completed calls and SDS are appended to a binary log and
    /// replayed on startup so history survives restarts.
    pub enabled: bool,
    /// Path to the append-only binary history log.
    pub path: PathBuf,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            path: PathBuf::from("brew-history.bin"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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

/// Configuration for the SIP subsystem: a UDP SIP listener that terminates
/// SIP extensions (user/pass registrations) and SIP trunks (peer gateways such
/// as Asterisk), plus the voice routes that bridge SIP to the Brew/TETRA side.
///
/// Trunks and extensions can be provisioned here in the TOML file *or* added at
/// runtime through the dashboard control API; runtime additions are held in
/// memory only and are lost on the config-file reload/restart, so anything that
/// must survive a restart belongs in the file.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SipConfig {
    /// Master switch for the whole SIP subsystem.
    pub enabled: bool,
    /// UDP address the SIP stack binds for signalling (default 0.0.0.0:5060).
    pub listen: SocketAddr,
    /// Address advertised to peers in Contact/Via when it must differ from
    /// `listen` (e.g. behind NAT). Empty means use the socket's local address.
    pub advertised_host: String,
    /// UDP port range for RTP media the relay allocates from, inclusive.
    pub rtp_port_min: u16,
    pub rtp_port_max: u16,
    /// SIP digest authentication realm presented to registering extensions.
    pub realm: String,
    /// Seconds a REGISTER binding is kept before it is considered expired when
    /// the client does not supply its own Expires.
    pub registration_ttl_seconds: u64,
    /// Statically provisioned SIP extensions (user/pass), keyed by the AOR user
    /// part (the number/name the extension registers as).
    pub extensions: HashMap<String, SipExtensionConfig>,
    /// Statically provisioned SIP trunks, keyed by an operator-chosen name.
    pub trunks: HashMap<String, SipTrunkConfig>,
    /// Voice routes bridging SIP and Brew endpoints. Evaluated top to bottom;
    /// the first matching route wins.
    pub routes: Vec<VoiceRouteConfig>,
}

impl Default for SipConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            listen: "0.0.0.0:5060".parse().unwrap(),
            advertised_host: String::new(),
            rtp_port_min: 16000,
            rtp_port_max: 17000,
            realm: "brew-server".into(),
            registration_ttl_seconds: 3600,
            extensions: HashMap::new(),
            trunks: HashMap::new(),
            routes: Vec::new(),
        }
    }
}

/// A provisioned SIP extension: a username/password the server authenticates on
/// REGISTER and INVITE. The extension's AOR user part is the map key.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SipExtensionConfig {
    /// Shared secret for SIP digest auth. Required in practice; an empty
    /// password disables authentication for this extension (not recommended).
    pub password: String,
    /// Optional human-readable label shown on the dashboard.
    pub display_name: String,
    /// Optional TETRA ISSI this extension maps to, so calls to/from the Brew
    /// side can address it as a subscriber. 0 means "not mapped".
    pub issi: u32,
    /// Whether this extension is allowed to place calls out to trunks.
    pub allow_outbound: bool,
}

impl Default for SipExtensionConfig {
    fn default() -> Self {
        Self {
            password: String::new(),
            display_name: String::new(),
            issi: 0,
            allow_outbound: true,
        }
    }
}

/// How a trunk associates with a remote peer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrunkDirection {
    /// The remote peer registers to us (we are the registrar). We learn its
    /// contact from its REGISTER; `remote_host` may be left empty.
    Inbound,
    /// We register to the remote peer and originate/receive calls to/from a
    /// fixed `remote_host`. Used for Asterisk/ITSP style trunks.
    Outbound,
    /// No registration in either direction; a static IP-authenticated peer
    /// identified purely by `remote_host`.
    Peer,
}

impl Default for TrunkDirection {
    fn default() -> Self { TrunkDirection::Peer }
}

/// A provisioned SIP trunk to a VoIP gateway (Asterisk, an ITSP, another PBX).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SipTrunkConfig {
    pub direction: TrunkDirection,
    /// `host:port` of the remote peer. Required for Outbound/Peer trunks;
    /// optional for Inbound trunks (filled in from the peer's REGISTER).
    pub remote_host: String,
    /// Auth username used when we register to / are challenged by the peer
    /// (Outbound), or the username the peer must use to register to us
    /// (Inbound). Defaults to the trunk name when empty.
    pub username: String,
    pub password: String,
    /// Auth realm expected from/presented to the peer. Empty uses the peer's
    /// challenged realm (Outbound) or the global SIP realm (Inbound).
    pub realm: String,
    /// Re-registration interval in seconds for Outbound trunks.
    pub register_interval_seconds: u64,
    /// Whether this trunk is currently enabled.
    pub enabled: bool,
}

impl Default for SipTrunkConfig {
    fn default() -> Self {
        Self {
            direction: TrunkDirection::default(),
            remote_host: String::new(),
            username: String::new(),
            password: String::new(),
            realm: String::new(),
            register_interval_seconds: 300,
            enabled: true,
        }
    }
}

/// One side of a voice route: which kind of endpoint, and its address.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RouteEndpoint {
    /// A SIP extension identified by its AOR user part.
    SipExtension { user: String },
    /// A SIP trunk identified by its configured name; `number` is the dialled
    /// number sent to / matched from the trunk.
    SipTrunk { trunk: String, #[serde(default)] number: String },
    /// A Brew private (individual) subscriber identified by ISSI.
    BrewPrivate { issi: u32 },
    /// A Brew group call identified by GSSI.
    BrewGroup { gssi: u32 },
}

/// A voice route rule. A call whose origin matches `from` and whose dialled
/// destination matches `match_pattern` is bridged to `to`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct VoiceRouteConfig {
    /// Operator label for the route.
    pub name: String,
    /// Glob-ish destination match against the dialled number/AOR. `*` matches
    /// any, a trailing `*` is a prefix match, otherwise exact.
    pub match_pattern: String,
    /// Endpoint the matching call is bridged to. Serialized as an inline table.
    pub to: Option<RouteEndpoint>,
    /// Optional restriction: only calls originating from this endpoint match.
    pub from: Option<RouteEndpoint>,
    /// A literal prefix stripped from the dialled string before it is handed
    /// to `to` (e.g. `to = { kind = "sip_trunk", trunk = "..." }` with an
    /// empty `number`, which passes the dialled string through as-is). Useful
    /// for a PBX-style outside-line prefix: `match_pattern = "9*"` selects
    /// the route on the leading "9", `strip_prefix = "9"` removes it so the
    /// trunk dials the bare number. Matching itself always runs against the
    /// *un-stripped* dialled string. Empty (the default) strips nothing.
    pub strip_prefix: String,
    pub enabled: bool,
}

impl Default for VoiceRouteConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            match_pattern: "*".into(),
            to: None,
            from: None,
            strip_prefix: String::new(),
            enabled: true,
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
            storage: StorageConfig::default(),
            sip: SipConfig::default(),
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

    /// Parses `text` as a config, same rules `load` applies to a file's
    /// contents. Used by the dashboard's config editor to validate a proposed
    /// change before writing it to disk.
    pub fn parse(text: &str) -> Result<Self> {
        toml::from_str(text).context("parsing config")
    }

    /// Renders this config back to TOML, the same shape `load` accepts. Used
    /// both to seed the dashboard's editor with the live config and to
    /// serialize a dashboard-made structured edit (e.g. one trunk added)
    /// before it is written to disk.
    pub fn to_toml_pretty(&self) -> Result<String> {
        toml::to_string_pretty(self).context("serializing config to TOML")
    }

    /// Atomically writes `text` to `path`: write to a sibling temp file, then
    /// rename over the target. A crash or concurrent read mid-write never
    /// observes a partial file, and the existing `config_watcher` (which polls
    /// the file's mtime) picks up the change as a single event.
    pub fn save_atomic(path: impl AsRef<Path>, text: &str) -> Result<()> {
        let path = path.as_ref();
        let tmp = path.with_extension("toml.tmp");
        fs::write(&tmp, text)
            .with_context(|| format!("writing {}", tmp.display()))?;
        fs::rename(&tmp, path)
            .with_context(|| format!("renaming {} to {}", tmp.display(), path.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_loads_when_file_missing() {
        let cfg = Config::load("/nonexistent/path/brew-server.toml").unwrap();
        assert_eq!(cfg.websocket_subprotocol, Config::default().websocket_subprotocol);
    }

    /// The dashboard config editor relies on serialize(edit)->parse being
    /// lossless for every shape actually used in a real config, including the
    /// trickiest bits: HashMap-keyed extensions/trunks and the internally
    /// tagged `RouteEndpoint` enum inside `Option`.
    #[test]
    fn to_toml_pretty_round_trips_through_parse() {
        let mut cfg = Config::default();
        cfg.sip.enabled = true;
        cfg.sip.extensions.insert("1001".into(), SipExtensionConfig {
            password: "secret".into(),
            display_name: "Front Desk".into(),
            issi: 42,
            allow_outbound: true,
        });
        cfg.sip.trunks.insert("asterisk".into(), SipTrunkConfig {
            direction: TrunkDirection::Outbound,
            remote_host: "10.0.0.5:5060".into(),
            username: "brew".into(),
            password: "hunter2".into(),
            realm: "asterisk".into(),
            register_interval_seconds: 120,
            enabled: true,
        });
        cfg.sip.routes.push(VoiceRouteConfig {
            name: "outbound".into(),
            match_pattern: "9*".into(),
            strip_prefix: "9".into(),
            to: Some(RouteEndpoint::SipTrunk { trunk: "asterisk".into(), number: "".into() }),
            from: Some(RouteEndpoint::BrewPrivate { issi: 42 }),
            enabled: true,
        });

        let text = cfg.to_toml_pretty().expect("serialize");
        let parsed = Config::parse(&text).expect("re-parse");

        assert_eq!(parsed.sip.enabled, true);
        assert_eq!(parsed.sip.extensions["1001"].issi, 42);
        assert_eq!(parsed.sip.trunks["asterisk"].direction, TrunkDirection::Outbound);
        assert_eq!(parsed.sip.routes.len(), 1);
        match &parsed.sip.routes[0].to {
            Some(RouteEndpoint::SipTrunk { trunk, .. }) => assert_eq!(trunk, "asterisk"),
            other => panic!("unexpected: {other:?}"),
        }
        match &parsed.sip.routes[0].from {
            Some(RouteEndpoint::BrewPrivate { issi }) => assert_eq!(*issi, 42),
            other => panic!("unexpected: {other:?}"),
        }
    }
}
