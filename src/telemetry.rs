//! FlowStation Telemetry channel: one-way BTS -> collector push of live
//! station state over a BTS-initiated WebSocket (subprotocol
//! `bluestation-telemetry-v2`). See flowstation-telemetry-control-api.md.
use crate::{fsnet, state::AppState};
use axum::extract::ws::{Message, WebSocket};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use tracing::{debug, info, warn};

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MsGroupInfo {
    pub gssi: u32,
    pub mnemonic: Option<String>,
    pub attachment_mode: Option<u8>,
    pub is_dynamic: bool,
    pub is_attached: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DgnaStatusInfo {
    pub issi: u32,
    pub gssi: u32,
    pub attach: bool,
    pub accepted: bool,
    pub source: String,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum HealthLevel {
    Ok,
    Degraded,
    Critical,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HealthDomain {
    Service,
    Backhaul,
    Radios,
    Congestion,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomainHealth {
    pub domain: HealthDomain,
    pub level: HealthLevel,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthSnapshot {
    pub overall: HealthLevel,
    pub domains: Vec<DomainHealth>,
    pub last_action: Option<String>,
    pub uptime_secs: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SysSensorKind {
    Temperature,
    Voltage,
    Current,
    Power,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SysSensor {
    pub name: String,
    pub kind: SysSensorKind,
    pub value: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TxVisual {
    pub sample_rate: f32,
    pub center_freq_hz: f64,
    pub rms_dbfs: f32,
    pub peak_dbfs: f32,
    pub spectrum_db_tenths: Vec<i16>,
    pub constellation_iq: Vec<i16>,
    pub carriers: Vec<(u16, f64)>,
    pub constellation_carrier: Option<(u16, f64)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TxQuality {
    pub papr_db: f32,
    pub evm_pct: f32,
    pub dc_offset_i: f32,
    pub dc_offset_q: f32,
    pub iq_amplitude_imbalance_db: f32,
    pub iq_phase_imbalance_deg: f32,
    pub carrier_leakage_db: f32,
    pub occupied_bandwidth_hz: f32,
    pub evm_carrier: Option<(u16, f64)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SdrHealth {
    pub temperature_c: Option<f32>,
    pub tx_gains: Vec<(String, f32)>,
    pub rx_gains: Vec<(String, f32)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SysHealthInfo {
    pub total_power_w: Option<f32>,
    pub sensors: Vec<SysSensor>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TelemetryEvent {
    MsRegistration { issi: u32 },
    MsDeregistration { issi: u32 },
    MsTimeoutDrop { issi: u32 },
    MsGroupAttach { issi: u32, gssis: Vec<u32> },
    MsGroupDetach { issi: u32, gssis: Vec<u32> },
    MsGroupsSnapshot { issi: u32, gssis: Vec<u32> },
    MsGroupCatalogSnapshot { issi: u32, groups: Vec<MsGroupInfo> },
    MsRssi { issi: u32, rssi_dbfs: f32 },
    MsEnergySaving { issi: u32, mode: u8 },
    DgnaStatus(DgnaStatusInfo),

    GroupCallStarted { call_id: u16, gssi: u32, caller_issi: u32, carrier_num: u16, ts: u8, priority: u8 },
    GroupCallEnded { call_id: u16, gssi: u32 },
    IndividualCallStarted {
        call_id: u16, calling_issi: u32, called_issi: u32, simplex: bool,
        carrier_num: u16, ts: u8,
        peer_carrier_num: Option<u16>, peer_ts: Option<u8>,
        priority: u8,
    },
    IndividualCallEnded { call_id: u16 },
    CallSpeakerChanged { call_id: u16, is_group: bool, dest_addr: u32, speaker_issi: u32, carrier_num: u16, ts: u8 },
    TsVoiceActivity { carrier_num: u16, ts: u8, speaker_issi: Option<u32> },

    SdsActivity { source_issi: u32, dest_issi: u32 },
    SdsLog { direction: String, source_issi: u32, dest_issi: u32, is_group: bool, protocol_id: u8, text: String },

    TxVisual(TxVisual),
    TxQuality(TxQuality),
    SdrHealth(SdrHealth),
    SysHealth(SysHealthInfo),
    HealthSnapshot(HealthSnapshot),

    EmergencyAlarm { source_issi: u32, dest_ssi: u32 },
    EmergencyCancel { source_issi: u32 },

    BrewConnected { connected: bool, server_version: u8 },
    DapnetLog { direction: String, id: String, callsign: String, recipient: String, text: String, priority: Option<u8>, paths: Vec<String> },
}

#[derive(Debug, Clone, Serialize)]
pub struct TelemetryCall {
    pub call_id: u16,
    pub is_group: bool,
    pub gssi_or_called: u32,
    pub source_issi: u32,
    pub carrier_num: u16,
    pub ts: u8,
    pub priority: u8,
    pub started_at_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SdsLogEntry {
    pub at_ms: u64,
    pub direction: String,
    pub source_issi: u32,
    pub dest_issi: u32,
    pub is_group: bool,
    pub protocol_id: u8,
    pub text: String,
}

/// Live, in-memory picture of one connected FlowStation BTS derived from its
/// telemetry stream. Resets when the connection drops (no persistence, mirrors
/// the rest of this server's dashboard state).
#[derive(Debug, Clone, Serialize)]
pub struct TelemetryBts {
    pub id: String,
    pub connected_at_ms: u64,
    pub last_event_at_ms: u64,
    pub health: Option<HealthSnapshot>,
    pub backhaul_connected: Option<bool>,
    #[serde(skip)]
    pub registrations: HashSet<u32>,
    pub registration_count: usize,
    /// Sorted list of registered subscriber ISSIs, derived from `registrations`.
    /// Exposed to the dashboard so it can show who is registered on this station
    /// (the `registrations` HashSet itself is skipped for stable JSON ordering).
    pub registrations_list: Vec<u32>,
    pub active_calls: HashMap<u16, TelemetryCall>,
    pub emergencies: HashSet<u32>,
    pub last_tx_quality: Option<TxQuality>,
    pub last_sdr_health: Option<SdrHealth>,
    pub last_sys_health: Option<SysHealthInfo>,
    #[serde(skip)]
    pub recent_sds: VecDeque<SdsLogEntry>,
    pub recent_sds_out: Vec<SdsLogEntry>,
    /// Latest decoded position per subscriber ISSI, from textual position
    /// beacons seen in the SDS stream on this station. Binary LIP beacons carry
    /// no recoverable coordinates (see the `position` module), so this only
    /// contains subscribers that beacon a textual position.
    #[serde(skip)]
    pub positions: HashMap<u32, MsPosition>,
    /// Serialized view of `positions` for the dashboard/map, newest first.
    pub positions_out: Vec<MsPosition>,
    /// Position beacons that were seen but could NOT be turned into coordinates,
    /// keyed by ISSI: binary LIP with no payload on the telemetry channel, or
    /// no-fix (all-zero) reports. Lets the dashboard show "beaconing but not
    /// plottable" instead of the radio appearing silent.
    #[serde(skip)]
    pub undecoded_beacons: HashMap<u32, UndecodedBeacon>,
    /// Serialized view of `undecoded_beacons`, newest first.
    pub undecoded_beacons_out: Vec<UndecodedBeacon>,
}

/// A position beacon that was observed but not decodable to coordinates.
#[derive(Debug, Clone, Serialize)]
pub struct UndecodedBeacon {
    pub issi: u32,
    pub at_ms: u64,
    pub count: u32,
    /// Why it could not be plotted (e.g. "binary LIP, no coordinates on
    /// telemetry channel").
    pub reason: String,
}

/// A decoded mobile-station position for the map.
#[derive(Debug, Clone, Serialize)]
pub struct MsPosition {
    pub issi: u32,
    pub lat: f64,
    pub lon: f64,
    pub at_ms: u64,
    /// The raw SDS text the coordinates were parsed from (for the popup).
    pub source_text: String,
}

impl TelemetryBts {
    fn new(id: String) -> Self {
        Self {
            id,
            connected_at_ms: now_ms(),
            last_event_at_ms: now_ms(),
            health: None,
            backhaul_connected: None,
            registrations: HashSet::new(),
            registration_count: 0,
            registrations_list: Vec::new(),
            active_calls: HashMap::new(),
            emergencies: HashSet::new(),
            last_tx_quality: None,
            last_sdr_health: None,
            last_sys_health: None,
            recent_sds: VecDeque::new(),
            recent_sds_out: Vec::new(),
            positions: HashMap::new(),
            positions_out: Vec::new(),
            undecoded_beacons: HashMap::new(),
            undecoded_beacons_out: Vec::new(),
        }
    }

    fn push_sds(&mut self, entry: SdsLogEntry) {
        // If the SDS body carries a textual position (APRS/decimal/Maidenhead),
        // record the latest coordinates for the sending subscriber so the map
        // can plot it. Binary LIP beacons (empty text) yield nothing here.
        if let Some(ll) = crate::position::parse_position(&entry.text) {
            self.positions.insert(entry.source_issi, MsPosition {
                issi: entry.source_issi,
                lat: ll.lat,
                lon: ll.lon,
                at_ms: entry.at_ms,
                source_text: entry.text.clone(),
            });
            self.undecoded_beacons.remove(&entry.source_issi);
            self.sync_positions();
            self.sync_undecoded();
        } else if entry.protocol_id == 10 {
            // A LIP position beacon arrived via telemetry, but with no decodable
            // coordinates (binary LIP payloads are stripped to empty text on the
            // telemetry channel; no-fix reports have no position). Track it so the
            // dashboard can show the radio is beaconing-but-not-plottable rather
            // than silent. If the same ISSI is already plotted from the Brew
            // channel, don't overwrite that — just note the beacon.
            let e = self.undecoded_beacons.entry(entry.source_issi).or_insert(UndecodedBeacon {
                issi: entry.source_issi,
                at_ms: entry.at_ms,
                count: 0,
                reason: "binary LIP / no coordinates on telemetry channel".to_string(),
            });
            e.at_ms = entry.at_ms;
            e.count = e.count.saturating_add(1);
            self.sync_undecoded();
        }
        self.recent_sds.push_front(entry);
        while self.recent_sds.len() > 50 {
            self.recent_sds.pop_back();
        }
        self.recent_sds_out = self.recent_sds.iter().cloned().collect();
    }

    /// Recomputes the serialized undecoded-beacon list (newest first).
    fn sync_undecoded(&mut self) {
        let mut list: Vec<UndecodedBeacon> = self.undecoded_beacons.values().cloned().collect();
        list.sort_unstable_by(|a, b| b.at_ms.cmp(&a.at_ms));
        self.undecoded_beacons_out = list;
    }

    /// Recomputes the serialized position list (newest first) after `positions`
    /// changes.
    fn sync_positions(&mut self) {
        let mut list: Vec<MsPosition> = self.positions.values().cloned().collect();
        list.sort_unstable_by(|a, b| b.at_ms.cmp(&a.at_ms));
        self.positions_out = list;
    }

    /// Recomputes the serialized registration view (count + sorted ISSI list)
    /// after the `registrations` set changes.
    fn sync_registrations(&mut self) {
        self.registration_count = self.registrations.len();
        let mut list: Vec<u32> = self.registrations.iter().copied().collect();
        list.sort_unstable();
        self.registrations_list = list;
    }
}

#[derive(Default)]
pub struct TelemetryState {
    pub stations: HashMap<String, TelemetryBts>,
    /// Positions decoded from the Brew SDS channel (LIP binary or text), keyed by
    /// subscriber ISSI. This is the primary source when FlowStation cannot be
    /// modified: the raw SDS (incl. LIP payloads) is relayed over the Brew
    /// protocol and decoded here, independent of the lossy telemetry SdsLog.
    pub sds_positions: HashMap<u32, PositionFix>,
}

impl TelemetryState {
    pub fn snapshot(&self) -> Vec<TelemetryBts> {
        self.stations.values().cloned().collect()
    }

    /// Flat list of the latest decoded MS positions across all stations, each
    /// tagged with the station that reported it, newest first. Only subscribers
    /// that beacon a *textual* position appear (see the `position` module).
    pub fn positions(&self) -> Vec<PositionFix> {
        // Merge Brew-channel SDS positions (primary) with any per-station
        // textual positions. On ISSI collision the newer fix wins.
        let mut by_issi: HashMap<u32, PositionFix> = self.sds_positions.clone();
        for s in self.stations.values() {
            for p in s.positions.values() {
                let fix = PositionFix {
                    issi: p.issi, lat: p.lat, lon: p.lon, at_ms: p.at_ms,
                    bts: s.id.clone(), source_text: p.source_text.clone(),
                };
                by_issi.entry(p.issi)
                    .and_modify(|e| if fix.at_ms >= e.at_ms { *e = fix.clone(); })
                    .or_insert(fix);
            }
        }
        let mut out: Vec<PositionFix> = by_issi.into_values().collect();
        out.sort_unstable_by(|a, b| b.at_ms.cmp(&a.at_ms));
        out
    }

    /// Records a position decoded from the Brew SDS channel for `issi`.
    pub fn record_sds_position(&mut self, issi: u32, lat: f64, lon: f64, at_ms: u64, source_text: String) {
        self.sds_positions.insert(issi, PositionFix {
            issi, lat, lon, at_ms, bts: "brew-sds".to_string(), source_text,
        });
        // This ISSI now has a real fix, so clear any "beaconing but not
        // plottable" markers for it across all stations.
        for s in self.stations.values_mut() {
            if s.undecoded_beacons.remove(&issi).is_some() {
                s.sync_undecoded();
            }
        }
    }
}

/// A position fix as served to the map, including which station reported it.
#[derive(Debug, Clone, Serialize)]
pub struct PositionFix {
    pub issi: u32,
    pub lat: f64,
    pub lon: f64,
    pub at_ms: u64,
    pub bts: String,
    pub source_text: String,
}

pub async fn run(state: Arc<AppState>) -> anyhow::Result<()> {
    if !state.config.telemetry.enabled {
        return Ok(());
    }
    let cfg = fsnet::ListenerConfig {
        name: "telemetry",
        listen: state.config.telemetry.listen,
        subprotocol: "bluestation-telemetry-v2",
        users: state.config.telemetry.users.clone(),
        tls: state.config.telemetry.tls.clone(),
    };
    fsnet::serve(cfg, move |socket, identity| {
        let state = state.clone();
        async move { session(state, socket, identity).await }
    })
    .await
}

async fn session(state: Arc<AppState>, socket: WebSocket, identity: Option<String>) {
    let id = identity.unwrap_or_else(|| format!("telemetry-{}", uuid::Uuid::new_v4().simple()));
    {
        let mut t = state.telemetry.write().await;
        t.stations.insert(id.clone(), TelemetryBts::new(id.clone()));
    }
    info!(bts = %id, "FlowStation telemetry connected");

    let (_tx, mut ws_rx) = socket.split();
    while let Some(item) = ws_rx.next().await {
        match item {
            Ok(Message::Binary(data)) => handle_event(&state, &id, &data).await,
            Ok(Message::Text(_)) => warn!(bts = %id, "unexpected text frame on telemetry channel, ignoring"),
            Ok(Message::Ping(_)) | Ok(Message::Pong(_)) => {}
            Ok(Message::Close(_)) => break,
            Err(e) => { warn!(bts = %id, error = %e, "telemetry WebSocket receive error"); break; }
        }
    }

    state.telemetry.write().await.stations.remove(&id);
    state.monitor.emit("telemetry_disconnected", serde_json::json!({"id": id}));
    info!(bts = %id, "FlowStation telemetry disconnected");
}

async fn handle_event(state: &Arc<AppState>, id: &str, data: &[u8]) {
    let event: TelemetryEvent = match serde_json::from_slice(data) {
        Ok(e) => e,
        Err(e) => {
            warn!(bts = %id, error = %e, bytes = data.len(), "dropping malformed telemetry event");
            return;
        }
    };
    debug!(bts = %id, ?event, "telemetry event");

    let mut t = state.telemetry.write().await;
    let Some(bts) = t.stations.get_mut(id) else { return };
    bts.last_event_at_ms = now_ms();

    // Only state-changing events wake the live dashboard socket; high-rate
    // instrumentation (TxQuality ~1/s, SdrHealth ~5s, SysHealth ~2s, TxVisual
    // ~5/s) is still recorded below but picked up by the existing 2s poll.
    let notify = !matches!(event, TelemetryEvent::TxVisual(_) | TelemetryEvent::TxQuality(_)
        | TelemetryEvent::SdrHealth(_) | TelemetryEvent::SysHealth(_)
        | TelemetryEvent::MsRssi { .. } | TelemetryEvent::TsVoiceActivity { .. });

    match event {
        TelemetryEvent::MsRegistration { issi } => { bts.registrations.insert(issi); bts.sync_registrations(); }
        TelemetryEvent::MsDeregistration { issi } | TelemetryEvent::MsTimeoutDrop { issi } => {
            bts.registrations.remove(&issi); bts.sync_registrations();
        }
        TelemetryEvent::GroupCallStarted { call_id, gssi, caller_issi, carrier_num, ts, priority } => {
            bts.active_calls.insert(call_id, TelemetryCall {
                call_id, is_group: true, gssi_or_called: gssi, source_issi: caller_issi,
                carrier_num, ts, priority, started_at_ms: now_ms(),
            });
        }
        TelemetryEvent::GroupCallEnded { call_id, .. } => { bts.active_calls.remove(&call_id); }
        TelemetryEvent::IndividualCallStarted { call_id, calling_issi, called_issi, carrier_num, ts, priority, .. } => {
            bts.active_calls.insert(call_id, TelemetryCall {
                call_id, is_group: false, gssi_or_called: called_issi, source_issi: calling_issi,
                carrier_num, ts, priority, started_at_ms: now_ms(),
            });
        }
        TelemetryEvent::IndividualCallEnded { call_id } => { bts.active_calls.remove(&call_id); }
        TelemetryEvent::SdsLog { direction, source_issi, dest_issi, is_group, protocol_id, text } => {
            info!(bts = %id, channel = "telemetry", source_issi, dest_issi, is_group, protocol_id,
                lip = (protocol_id == 10), has_text = !text.trim().is_empty(),
                "SDS ({direction})");
            bts.push_sds(SdsLogEntry { at_ms: now_ms(), direction, source_issi, dest_issi, is_group, protocol_id, text });
        }
        TelemetryEvent::TxQuality(q) => bts.last_tx_quality = Some(q),
        TelemetryEvent::SdrHealth(h) => bts.last_sdr_health = Some(h),
        TelemetryEvent::SysHealth(h) => bts.last_sys_health = Some(h),
        TelemetryEvent::HealthSnapshot(h) => bts.health = Some(h),
        TelemetryEvent::EmergencyAlarm { source_issi, .. } => { bts.emergencies.insert(source_issi); }
        TelemetryEvent::EmergencyCancel { source_issi } => { bts.emergencies.remove(&source_issi); }
        TelemetryEvent::BrewConnected { connected, .. } => bts.backhaul_connected = Some(connected),
        _ => {}
    }
    drop(t);
    if notify {
        state.monitor.emit("telemetry", serde_json::json!({"id": id}));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sds(source_issi: u32, protocol_id: u8, text: &str, at_ms: u64) -> SdsLogEntry {
        SdsLogEntry {
            at_ms,
            direction: "rx".to_string(),
            source_issi,
            dest_issi: 0,
            is_group: false,
            protocol_id,
            text: text.to_string(),
        }
    }

    #[test]
    fn textual_position_beacon_is_stored() {
        let mut bts = TelemetryBts::new("bts-1".to_string());
        bts.push_sds(sds(1001, 3, "44.4353, 26.1092", 100));
        assert_eq!(bts.positions.len(), 1);
        let p = &bts.positions[&1001];
        assert!((p.lat - 44.4353).abs() < 0.01 && (p.lon - 26.1092).abs() < 0.01);
        assert_eq!(bts.positions_out.len(), 1);
    }

    #[test]
    fn empty_lip_beacon_stores_no_position() {
        let mut bts = TelemetryBts::new("bts-1".to_string());
        // PID 10 with empty text (the binary-LIP case) yields no coordinates.
        bts.push_sds(sds(1001, 10, "", 100));
        assert!(bts.positions.is_empty());
        assert!(bts.positions_out.is_empty());
    }

    #[test]
    fn latest_position_replaces_older_for_same_issi() {
        let mut bts = TelemetryBts::new("bts-1".to_string());
        bts.push_sds(sds(1001, 3, "44.00, 26.00", 100));
        bts.push_sds(sds(1001, 3, "45.00, 27.00", 200));
        assert_eq!(bts.positions.len(), 1);
        let p = &bts.positions[&1001];
        assert!((p.lat - 45.0).abs() < 0.01, "should keep newest");
        assert_eq!(p.at_ms, 200);
    }

    #[test]
    fn positions_across_stations_are_aggregated_and_tagged() {
        let mut state = TelemetryState::default();
        let mut a = TelemetryBts::new("BTS-A".to_string());
        a.push_sds(sds(1, 3, "44.0, 26.0", 100));
        let mut b = TelemetryBts::new("BTS-B".to_string());
        b.push_sds(sds(2, 3, "45.0, 27.0", 200));
        state.stations.insert("BTS-A".to_string(), a);
        state.stations.insert("BTS-B".to_string(), b);
        let fixes = state.positions();
        assert_eq!(fixes.len(), 2);
        // newest first
        assert_eq!(fixes[0].issi, 2);
        assert_eq!(fixes[0].bts, "BTS-B");
        assert!(fixes.iter().any(|f| f.issi == 1 && f.bts == "BTS-A"));
    }

    #[test]
    fn sync_registrations_sorts_and_counts() {
        let mut bts = TelemetryBts::new("bts-1".to_string());
        bts.registrations.insert(300);
        bts.registrations.insert(100);
        bts.registrations.insert(200);
        bts.sync_registrations();
        assert_eq!(bts.registration_count, 3);
        assert_eq!(bts.registrations_list, vec![100, 200, 300]);
    }

    #[test]
    fn sync_registrations_after_removal() {
        let mut bts = TelemetryBts::new("bts-1".to_string());
        for issi in [10u32, 20, 30] { bts.registrations.insert(issi); }
        bts.sync_registrations();
        bts.registrations.remove(&20);
        bts.sync_registrations();
        assert_eq!(bts.registration_count, 2);
        assert_eq!(bts.registrations_list, vec![10, 30]);
    }

    #[test]
    fn registrations_list_serialized_in_snapshot() {
        let mut bts = TelemetryBts::new("bts-1".to_string());
        bts.registrations.insert(4242);
        bts.sync_registrations();
        let json = serde_json::to_string(&bts).unwrap();
        assert!(json.contains("registrations_list"), "list must be serialized");
        assert!(json.contains("4242"));
        // the raw HashSet field stays skipped
        assert!(!json.contains("\"registrations\":"), "raw set must be skipped");
    }
}
