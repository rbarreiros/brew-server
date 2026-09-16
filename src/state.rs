use crate::{config::Config, control::ControlState, monitor::Monitor, protocol::ConnVersion, telemetry::TelemetryState};
use std::{collections::{HashMap, HashSet}, time::{Duration, Instant}};
use tokio::sync::{mpsc, RwLock};
use uuid::Uuid;

pub type ClientId = Uuid;

/// Brew client role advertised via the `X-Brew-Mode` HTTP header at discovery.
/// Per the specification a `Terminal` does not need registration updates pushed
/// from the server, whereas a `Basestation` does. Defaults to `Basestation`
/// (the conservative choice) when the header is absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ClientMode {
    Terminal,
    #[default]
    Basestation,
}

impl ClientMode {
    pub fn from_header(value: Option<&str>) -> Self {
        match value.map(|v| v.trim()) {
            Some(v) if v.eq_ignore_ascii_case("Terminal") => ClientMode::Terminal,
            _ => ClientMode::Basestation,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ClientMode::Terminal => "Terminal",
            ClientMode::Basestation => "Basestation",
        }
    }

    /// Whether the server should push subscriber-registration updates to this
    /// client. Terminals opt out to save resources, per the spec.
    pub fn wants_registration_updates(self) -> bool {
        matches!(self, ClientMode::Basestation)
    }
}

#[derive(Clone)]
pub struct Client {
    pub tx: mpsc::UnboundedSender<Vec<u8>>,
    pub mode: ClientMode,
    /// Negotiated Brew protocol version for this connection. Seeded from the
    /// discovery `X-Brew-Version` header (if any) and promoted lazily as v1
    /// message layouts are observed on the wire.
    pub version: ConnVersion,
}

#[derive(Debug, Clone)]
pub struct Subscriber {
    pub client_id: ClientId,
    pub groups: HashSet<u32>,
    /// The connection mode of the client that registered this ISSI (Terminal
    /// or Basestation). Only `Terminal` connections represent an actual mobile
    /// station; a `Basestation` (BlueStation gateway) registering on a
    /// subscriber's behalf is not itself an MS. Dashboard MS-registration
    /// counts should filter on this.
    pub mode: ClientMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallKind {
    Group,
    Private,
}

#[derive(Debug, Clone)]
pub struct ActiveCall {
    pub kind: CallKind,
    pub owner: ClientId,
    pub source_issi: u32,
    pub destination: u32,
    pub priority: u8,
    pub peers: HashSet<ClientId>,
}

#[derive(Debug, Clone)]
pub struct SdsRoute {
    pub source_client: ClientId,
    pub targets: HashSet<ClientId>,
    pub source_issi: u32,
    pub destination: u32,
    pub created_at: Instant,
}

#[derive(Default)]
pub struct Inner {
    pub clients: HashMap<ClientId, Client>,
    pub subscribers: HashMap<u32, Subscriber>,
    pub group_clients: HashMap<u32, HashSet<ClientId>>,
    pub calls: HashMap<Uuid, ActiveCall>,
    pub group_floor: HashMap<u32, Uuid>,
    pub sds_routes: HashMap<Uuid, SdsRoute>,
    pub digest_nonces: HashMap<String, Instant>,
    pub auth_sessions: HashMap<String, (Instant, ClientMode, ConnVersion)>,
}

impl Inner {
    /// Number of registered subscribers that represent an actual mobile
    /// station, i.e. registered by a `Terminal`-mode client. A `Basestation`
    /// (BlueStation gateway) can also hold a subscriber registration, but it
    /// is not itself an MS, so it is excluded from MS-registration counts.
    pub fn ms_registration_count(&self) -> usize {
        self.subscribers.values().filter(|s| s.mode == ClientMode::Terminal).count()
    }
}

#[cfg(test)]
mod ms_registration_tests {
    use super::*;

    fn subscriber(mode: ClientMode) -> Subscriber {
        Subscriber { client_id: Uuid::new_v4(), groups: HashSet::new(), mode }
    }

    #[test]
    fn counts_only_terminal_mode_subscribers() {
        let mut inner = Inner::default();
        inner.subscribers.insert(1001, subscriber(ClientMode::Terminal));
        inner.subscribers.insert(1002, subscriber(ClientMode::Terminal));
        inner.subscribers.insert(2001, subscriber(ClientMode::Basestation));
        assert_eq!(inner.ms_registration_count(), 2);
        assert_eq!(inner.subscribers.len(), 3, "raw map still holds every registration");
    }

    #[test]
    fn zero_when_only_basestations_registered() {
        let mut inner = Inner::default();
        inner.subscribers.insert(2001, subscriber(ClientMode::Basestation));
        inner.subscribers.insert(2002, subscriber(ClientMode::Basestation));
        assert_eq!(inner.ms_registration_count(), 0);
    }

    #[test]
    fn zero_when_no_subscribers() {
        assert_eq!(Inner::default().ms_registration_count(), 0);
    }
}

pub struct AppState {
    pub config: Config,
    pub inner: RwLock<Inner>,
    pub monitor: Monitor,
    pub telemetry: RwLock<TelemetryState>,
    pub control: RwLock<ControlState>,
}

impl AppState {
    pub fn new(config: Config) -> Self {
        let store = if config.storage.enabled {
            match crate::store::Store::open(&config.storage.path) {
                Ok(store) => Some(std::sync::Arc::new(store)),
                Err(e) => {
                    tracing::error!(error = %e, path = %config.storage.path.display(), "cannot open history store; continuing without persistence");
                    None
                }
            }
        } else {
            None
        };
        let monitor = match &store {
            Some(store) => Monitor::with_store(store.clone()),
            None => Monitor::new(),
        };
        let telemetry = match &store {
            Some(store) => TelemetryState::with_store(store.clone()),
            None => TelemetryState::default(),
        };
        Self {
            config,
            inner: RwLock::new(Inner::default()),
            monitor,
            telemetry: RwLock::new(telemetry),
            control: RwLock::new(ControlState::default()),
        }
    }

    /// Returns the current negotiated version for a connection (defaulting to
    /// V0 for unknown clients).
    pub async fn client_version(&self, id: ClientId) -> ConnVersion {
        self.inner.read().await.clients.get(&id).map(|c| c.version).unwrap_or_default()
    }

    /// Promotes a connection's stored version to at least `observed`, returning
    /// true if this raised the version (so callers can log the transition once).
    pub async fn promote_client_version(&self, id: ClientId, observed: ConnVersion) -> bool {
        let mut inner = self.inner.write().await;
        if let Some(client) = inner.clients.get_mut(&id) {
            if observed.as_u8() > client.version.as_u8() {
                client.version = observed;
                return true;
            }
        }
        false
    }

    pub async fn send_many(&self, clients: &HashSet<ClientId>, packet: &[u8]) {
        let inner = self.inner.read().await;
        for id in clients {
            if let Some(client) = inner.clients.get(id) {
                let _ = client.tx.send(packet.to_vec());
            }
        }
    }

    pub async fn purge_ephemeral(&self) {
        let now = Instant::now();
        let session_ttl = Duration::from_secs(self.config.auth.session_ttl_seconds.max(1));
        let mut inner = self.inner.write().await;
        inner.digest_nonces.retain(|_, at| now.duration_since(*at) < Duration::from_secs(120));
        inner.auth_sessions.retain(|_, (at, _, _)| now.duration_since(*at) < session_ttl);
        inner.sds_routes.retain(|_, route| now.duration_since(route.created_at) < Duration::from_secs(60));
    }

    pub async fn cleanup_client(&self, id: ClientId) {
        let mut inner = self.inner.write().await;
        inner.clients.remove(&id);

        let removed_issis: Vec<u32> = inner.subscribers.iter()
            .filter_map(|(issi, sub)| (sub.client_id == id).then_some(*issi)).collect();
        for issi in removed_issis { inner.subscribers.remove(&issi); }

        for clients in inner.group_clients.values_mut() { clients.remove(&id); }
        inner.group_clients.retain(|_, clients| !clients.is_empty());

        let removed_calls: Vec<Uuid> = inner.calls.iter()
            .filter_map(|(uuid, call)| (call.owner == id || call.peers.contains(&id)).then_some(*uuid)).collect();
        for uuid in removed_calls {
            if let Some(call) = inner.calls.remove(&uuid) {
                if call.kind == CallKind::Group && inner.group_floor.get(&call.destination) == Some(&uuid) {
                    inner.group_floor.remove(&call.destination);
                }
            }
        }
        inner.sds_routes.retain(|_, route| route.source_client != id && !route.targets.contains(&id));
    }
}
