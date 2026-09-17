//! Bridge between the SIP subsystem and the Brew/TETRA core.
//!
//! This is the single place the two subsystems meet. It maps a routed SIP call
//! onto a Brew call: a SIP->Brew-private call becomes a Brew private
//! (individual) call setup toward the ISSI's registered BlueStation, and a
//! SIP->Brew-group call becomes a group transmission to a GSSI reaching the
//! affiliated basestation mobile stations and brew mobile clients.
//!
//! Media reality check: TETRA carries ACELP voice inside Brew traffic frames,
//! while SIP legs here are steered to G.711 (PCMU/PCMA). To bridge media end
//! to end we register a *virtual* Brew client for the duration of the call: it
//! has no socket of its own, but sits in `AppState` exactly like a real
//! Basestation connection (in `clients`, as an `ActiveCall` participant, and
//! for group calls in `group_clients`), so the existing router delivers voice
//! frames to it like any other peer. A `transcode::task` owns that virtual
//! client's receive side on one end and the SIP RTP leg on the other, running
//! the vendored ACELP codec (`transcode::acelp`) and G.711 (`transcode::g711`)
//! between them.

use crate::protocol::{self, ConnVersion};
use crate::sip::media::{RtpRelay, Sdp};
use crate::sip::message::{extract_uri, uri_user, SipMessage};
use crate::sip::state::SipCall;
use crate::sip::transport::SipTransport;
use crate::state::{ActiveCall, AppState, CallKind, Client, ClientId, ClientMode};
use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tracing::{info, warn};

/// Identifies the real Brew call/participant a Brew->SIP bridge is placed on
/// behalf of, so `place_outbound` can hook a transcoder into the same
/// `ActiveCall` id the subscriber's traffic frames are already tagged with.
pub struct BrewCallLink {
    pub call_id: uuid::Uuid,
    pub client: ClientId,
}

/// Enough of a SIP dialog to send a best-effort in-dialog BYE toward the
/// other party when the *Brew* side hangs up first (rather than the usual
/// SIP BYE/CANCEL). Dialog tracking in this bridge is intentionally minimal
/// (see `terminate_to_extension`'s note on the same simplification): this
/// reuses the exact From/To header values already exchanged rather than
/// tracking full RFC 3261 dialog state (route sets, remote CSeq, etc).
#[derive(Clone)]
struct DialogBye {
    request_uri: String,
    target_addr: SocketAddr,
    from: String,
    to: String,
}

/// Bookkeeping for one bridged call's virtual Brew participant + transcoder,
/// so it can be torn down when the SIP call ends (BYE/CANCEL) or when the
/// Brew side ends it first (CALL_RELEASE/CALL_GROUP_IDLE).
struct BridgedLeg {
    virtual_client: ClientId,
    brew_call_id: uuid::Uuid,
    group: Option<u32>,
    task: tokio::task::JoinHandle<()>,
    /// Set when this bridge placed the outbound/inbound SIP dialog itself
    /// (i.e. every case here), so a Brew-initiated hangup can notify the SIP
    /// peer instead of leaving its dialog dangling.
    bye: Option<DialogBye>,
}

/// Couples the SIP transport to the Brew core.
pub struct BrewBridge {
    app: Arc<AppState>,
    transport: Arc<SipTransport>,
    /// SIP Call-ID -> bridged-leg cleanup info, for calls this bridge placed
    /// (both directions). Entries are removed by `teardown`.
    legs: RwLock<std::collections::HashMap<String, BridgedLeg>>,
}

impl BrewBridge {
    pub fn new(app: Arc<AppState>, transport: Arc<SipTransport>) -> Self {
        Self { app, transport, legs: RwLock::new(std::collections::HashMap::new()) }
    }

    /// Negotiated G.711 payload type (0=PCMU, 8=PCMA) to run the transcoder
    /// at, matching whatever `Sdp::build`'s answer actually offered.
    fn transcoder_payload_type(payloads: &[u8]) -> u8 {
        payloads.iter().copied().find(|p| *p == 0 || *p == 8).unwrap_or(0)
    }

    /// Tears down a bridged call's virtual Brew participant: aborts the
    /// transcoder task, removes the virtual client from every place it was
    /// registered (connection table, active call, group affiliation), and
    /// notifies the real Brew participant(s) the call ended (a synthesized
    /// CALL_RELEASE/CALL_GROUP_IDLE) so their UI doesn't show a phantom
    /// in-progress call. Called from SIP BYE/CANCEL handling; safe to call
    /// for a call this bridge did not place (no-op).
    pub async fn teardown(&self, call_id: &str) {
        let Some(leg) = self.legs.write().await.remove(call_id) else { return };
        leg.task.abort();
        let mut inner = self.app.inner.write().await;
        inner.clients.remove(&leg.virtual_client);
        let call = inner.calls.remove(&leg.brew_call_id);
        if let Some(gssi) = leg.group {
            if let Some(members) = inner.group_clients.get_mut(&gssi) {
                members.remove(&leg.virtual_client);
            }
            if inner.group_floor.get(&gssi) == Some(&leg.brew_call_id) {
                inner.group_floor.remove(&gssi);
            }
        }
        let notify = call.map(|call| {
            let mut targets = call.peers.clone();
            targets.insert(call.owner);
            targets.remove(&leg.virtual_client);
            let release_state = if call.kind == CallKind::Group { protocol::CALL_GROUP_IDLE } else { protocol::CALL_RELEASE };
            let msg = protocol::build_call_cause(release_state, &leg.brew_call_id, 0);
            let txs = targets.iter().filter_map(|c| inner.clients.get(c).map(|cl| cl.tx.clone())).collect::<Vec<_>>();
            (msg, txs)
        });
        drop(inner);
        if let Some((msg, txs)) = notify {
            for tx in txs { let _ = tx.send(msg.clone()); }
        }
    }

    /// Called when the Brew core ends a call (CALL_RELEASE/CALL_GROUP_IDLE)
    /// that turns out to be one this bridge placed toward SIP: the Brew side
    /// has already been told (that's how we got here), so this only needs to
    /// notify the SIP peer with a best-effort BYE and clean up our side.
    pub async fn teardown_by_brew_call(&self, brew_call_id: uuid::Uuid) {
        let call_id = {
            let legs = self.legs.read().await;
            legs.iter().find(|(_, l)| l.brew_call_id == brew_call_id).map(|(k, _)| k.clone())
        };
        let Some(call_id) = call_id else { return };
        if let Some(bye) = self.legs.read().await.get(&call_id).and_then(|l| l.bye.clone()) {
            self.send_bye(&call_id, &bye).await;
        }
        // `call` is already gone from AppState (the Brew side removed it
        // before calling us), so this only aborts the task and cleans the
        // virtual-client bookkeeping; it will not re-notify Brew.
        self.teardown(&call_id).await;
    }

    async fn send_bye(&self, call_id: &str, d: &DialogBye) {
        use crate::sip::message::Method;
        let mut bye = SipMessage::new_request(Method::Bye, d.request_uri.clone());
        bye.push_header("Via", format!("SIP/2.0/UDP {};branch=z9hG4bK{}",
            self.transport.advertised_host, uuid::Uuid::new_v4().simple()));
        bye.push_header("Max-Forwards", "70");
        bye.push_header("From", d.from.clone());
        bye.push_header("To", d.to.clone());
        bye.push_header("Call-ID", call_id.to_string());
        bye.push_header("CSeq", "2 BYE");
        self.transport.send_to(&bye, d.target_addr).await;
        info!(%call_id, "Brew hangup: sent SIP BYE");
    }

    /// Answers the SIP caller and sets up a Brew *private* call to `issi`.
    ///
    /// Signalling: we locate the BlueStation that owns `issi` (its registered
    /// subscriber) and record an active call so the panel shows it. Media: we
    /// allocate one relay leg toward the SIP caller; the Brew side of the media
    /// path is where the ACELP<->PCM transcoder attaches.
    pub async fn sip_to_brew_private(
        &self,
        caller: SocketAddr,
        req: &SipMessage,
        issi: u32,
        offer: &Sdp,
        payloads: &[u8],
        call_id: &str,
    ) {
        // Is the target ISSI reachable (registered on some BlueStation)?
        let target_client = {
            let inner = self.app.inner.read().await;
            inner.subscribers.get(&issi).map(|s| s.client_id)
        };
        let Some(target_client) = target_client else {
            warn!(issi, %call_id, "SIP->Brew private: ISSI not registered");
            let resp = self.transport.base_response_pub(req, 480, "Temporarily Unavailable");
            self.transport.send_to(&resp, caller).await;
            self.transport.state.end_call(call_id).await;
            return;
        };

        // Allocate the SIP-facing relay leg and latch the caller's media addr.
        let leg = match self.transport.relay.alloc_leg().await {
            Ok(l) => l,
            Err(_) => {
                let resp = self.transport.base_response_pub(req, 500, "Server Internal Error");
                self.transport.send_to(&resp, caller).await;
                self.transport.state.end_call(call_id).await;
                return;
            }
        };
        if let Ok(addr) = format!("{}:{}", offer.connection_addr, offer.audio_port).parse::<SocketAddr>() {
            leg.set_remote(addr).await;
        }
        self.transport.state.set_call_rtp(call_id, Some(leg.local_port), None).await;

        // Media bridge point: register a virtual Brew client standing in for
        // the SIP caller, wire it into an ActiveCall with the real ISSI's
        // client as its sole peer (so the router delivers that peer's voice
        // frames to our virtual client exactly like a normal private call),
        // ring the ISSI with a synthesized SETUP_REQUEST, and start the
        // transcoder between `leg` and the virtual client's frame channel.
        let brew_call_id = uuid::Uuid::new_v4();
        let virtual_client = uuid::Uuid::new_v4();
        let (virtual_tx, virtual_rx) = mpsc::unbounded_channel();
        let target_tx = {
            let mut inner = self.app.inner.write().await;
            inner.clients.insert(virtual_client, Client { tx: virtual_tx, mode: ClientMode::Terminal, version: ConnVersion::V1 });
            inner.calls.insert(brew_call_id, ActiveCall {
                kind: CallKind::Private,
                owner: virtual_client,
                source_issi: 0,
                destination: issi,
                priority: 0,
                peers: HashSet::from([target_client]),
            });
            inner.clients.get(&target_client).map(|c| c.tx.clone())
        };
        if let Some(tx) = &target_tx {
            let setup = protocol::build_circular_call_setup(&brew_call_id, 0, issi, 0);
            let _ = tx.send(setup);
        }
        let local_port = leg.local_port;
        let mut ok = self.transport.base_response_pub(req, 200, "OK");
        ok.push_header("Contact", format!("<sip:brew@{}>", self.transport.advertised_host));
        ok.push_header("Content-Type", "application/sdp");
        ok.body = Sdp::build(&self.transport.advertised_host, local_port, payloads);
        // Capture enough of this dialog (our to-tag, their from-tag) to send
        // a BYE toward `caller` later if the Brew side hangs up first.
        let bye = match (ok.header("to"), ok.header("from"), req.header("contact")) {
            (Some(to), Some(from), contact) => Some(DialogBye {
                request_uri: contact.map(extract_uri).unwrap_or_else(|| format!("sip:{caller}")),
                target_addr: caller,
                from: to.to_string(),
                to: from.to_string(),
            }),
            _ => None,
        };

        let payload_type = Self::transcoder_payload_type(payloads);
        let task = crate::transcode::task::spawn(
            leg, payload_type, brew_call_id, virtual_rx,
            target_tx.into_iter().collect(),
        );
        self.legs.write().await.insert(call_id.to_string(), BridgedLeg {
            virtual_client, brew_call_id, group: None, task, bye,
        });

        self.transport.send_to(&ok, caller).await;
        self.transport.state.answer_call(call_id).await;
        info!(issi, %call_id, "bridged SIP call to Brew private (media: ACELP<->G.711 transcoder active)");
    }

    /// Answers the SIP caller and sets up a Brew *group* call to `gssi`,
    /// reaching affiliated basestation mobile stations and brew mobile clients.
    pub async fn sip_to_brew_group(
        &self,
        caller: SocketAddr,
        req: &SipMessage,
        gssi: u32,
        offer: &Sdp,
        payloads: &[u8],
        call_id: &str,
    ) {
        // How many clients are affiliated to this group right now?
        let affiliated = {
            let inner = self.app.inner.read().await;
            inner.group_clients.get(&gssi).map(|c| c.len()).unwrap_or(0)
        };
        if affiliated == 0 && !self.app.config.fallback_broadcast_when_no_affiliations {
            warn!(gssi, %call_id, "SIP->Brew group: no affiliations");
            let resp = self.transport.base_response_pub(req, 480, "Temporarily Unavailable");
            self.transport.send_to(&resp, caller).await;
            self.transport.state.end_call(call_id).await;
            return;
        }

        let leg = match self.transport.relay.alloc_leg().await {
            Ok(l) => l,
            Err(_) => {
                let resp = self.transport.base_response_pub(req, 500, "Server Internal Error");
                self.transport.send_to(&resp, caller).await;
                self.transport.state.end_call(call_id).await;
                return;
            }
        };
        if let Ok(addr) = format!("{}:{}", offer.connection_addr, offer.audio_port).parse::<SocketAddr>() {
            leg.set_remote(addr).await;
        }
        self.transport.state.set_call_rtp(call_id, Some(leg.local_port), None).await;

        // Media bridge point: register a virtual Brew client as an affiliated
        // member of the group (so it keeps receiving that group's traffic for
        // the life of the call, exactly like a real Basestation), seize the
        // floor with a synthesized GROUP_TX toward the currently-affiliated
        // members, and start the transcoder between `leg` and the virtual
        // client's frame channel.
        let brew_call_id = uuid::Uuid::new_v4();
        let virtual_client = uuid::Uuid::new_v4();
        let (virtual_tx, virtual_rx) = mpsc::unbounded_channel();
        let target_txs = {
            let mut inner = self.app.inner.write().await;
            inner.clients.insert(virtual_client, Client { tx: virtual_tx, mode: ClientMode::Terminal, version: ConnVersion::V1 });
            let members = inner.group_clients.entry(gssi).or_default();
            members.insert(virtual_client);
            let targets: HashSet<ClientId> = members.iter().copied().filter(|c| *c != virtual_client).collect();
            inner.calls.insert(brew_call_id, ActiveCall {
                kind: CallKind::Group,
                owner: virtual_client,
                source_issi: 0,
                destination: gssi,
                priority: 0,
                peers: targets.clone(),
            });
            inner.group_floor.insert(gssi, brew_call_id);
            targets.iter().filter_map(|c| inner.clients.get(c).map(|cl| cl.tx.clone())).collect::<Vec<_>>()
        };
        let seize = protocol::build_group_tx(&brew_call_id, 0, gssi, 0);
        for tx in &target_txs { let _ = tx.send(seize.clone()); }

        let local_port = leg.local_port;
        let mut ok = self.transport.base_response_pub(req, 200, "OK");
        ok.push_header("Contact", format!("<sip:brew@{}>", self.transport.advertised_host));
        ok.push_header("Content-Type", "application/sdp");
        ok.body = Sdp::build(&self.transport.advertised_host, local_port, payloads);
        let bye = match (ok.header("to"), ok.header("from"), req.header("contact")) {
            (Some(to), Some(from), contact) => Some(DialogBye {
                request_uri: contact.map(extract_uri).unwrap_or_else(|| format!("sip:{caller}")),
                target_addr: caller,
                from: to.to_string(),
                to: from.to_string(),
            }),
            _ => None,
        };

        let payload_type = Self::transcoder_payload_type(payloads);
        let task = crate::transcode::task::spawn(leg, payload_type, brew_call_id, virtual_rx, target_txs);
        self.legs.write().await.insert(call_id.to_string(), BridgedLeg {
            virtual_client, brew_call_id, group: Some(gssi), task, bye,
        });

        self.transport.send_to(&ok, caller).await;
        self.transport.state.answer_call(call_id).await;
        info!(gssi, affiliated, %call_id, "bridged SIP call to Brew group (media: ACELP<->G.711 transcoder active)");
    }

    /// Brew -> SIP direction: called by the Brew core when a TETRA subscriber or
    /// group originates a call whose destination resolves (via routes) to a SIP
    /// extension or trunk. Places the outbound SIP INVITE and tracks the call.
    ///
    /// This is invoked opportunistically; if SIP is disabled or no route
    /// matches, it is a no-op returning false.
    pub async fn brew_to_sip(
        &self,
        origin: crate::sip::routing::CallOrigin,
        dialled: &str,
        link: BrewCallLink,
    ) -> bool {
        let routes = &self.app.config.sip.routes;
        let Some((dest, route)) = crate::sip::routing::resolve(routes, &origin, dialled) else {
            return false;
        };
        info!(dialled = %dialled, route = %route.name, "Brew->SIP route matched");

        // Only extension/trunk destinations make sense coming from Brew.
        use crate::sip::state::LegEndpoint;
        let call_id = format!("brew-{}", uuid::Uuid::new_v4().simple());
        match dest.clone() {
            LegEndpoint::SipExtension { aor } => {
                let Some(reg) = self.transport.state.lookup_registration(&aor).await else {
                    warn!(aor = %aor, "Brew->SIP: destination extension not registered");
                    return false;
                };
                self.place_outbound(&call_id, origin.to_leg(), dest.clone(), reg.contact, reg.source, link).await;
                true
            }
            LegEndpoint::SipTrunk { trunk, number } => {
                let Some(tc) = self.app.config.sip.trunks.get(&trunk).cloned() else { return false };
                let Some(peer) = tc.remote_host.parse::<SocketAddr>().ok()
                    .or(self.transport.state.trunk_for_peer_addr(&trunk).await) else { return false };
                let host = tc.remote_host.split(':').next().unwrap_or(&tc.remote_host);
                let uri = format!("sip:{number}@{host}");
                self.place_outbound(&call_id, origin.to_leg(), dest.clone(), uri, peer, link).await;
                true
            }
            _ => false,
        }
    }

    /// Shared helper: allocate a relay leg, INVITE the SIP destination, track
    /// the call, and attach the transcoder. Media bridge point: registers a
    /// virtual Brew client as this call's SIP-side participant (peer of
    /// `link.client`, the real subscriber connection that originated the
    /// call), so the router delivers the subscriber's voice frames to it
    /// exactly like sip_to_brew_private's reverse case.
    async fn place_outbound(
        &self,
        call_id: &str,
        from: crate::sip::state::LegEndpoint,
        to: crate::sip::state::LegEndpoint,
        target_uri: String,
        target_addr: SocketAddr,
        link: BrewCallLink,
    ) {
        let leg = match self.transport.relay.alloc_leg().await {
            Ok(l) => l,
            Err(_) => { warn!(%call_id, "Brew->SIP: no RTP port"); return; }
        };
        self.transport.state.start_call(SipCall {
            call_id: call_id.to_string(),
            from,
            to,
            started_at_ms: now_ms(),
            answered_at_ms: None,
            state: "brew-originated".into(),
            rtp_a_port: Some(leg.local_port),
            rtp_b_port: None,
        }).await;

        let virtual_client = uuid::Uuid::new_v4();
        let (virtual_tx, virtual_rx) = mpsc::unbounded_channel();
        let brew_target_tx = {
            let mut inner = self.app.inner.write().await;
            inner.clients.insert(virtual_client, Client { tx: virtual_tx, mode: ClientMode::Terminal, version: ConnVersion::V1 });
            inner.calls.insert(link.call_id, ActiveCall {
                kind: CallKind::Private,
                owner: link.client,
                source_issi: 0,
                destination: 0,
                priority: 0,
                peers: HashSet::from([virtual_client]),
            });
            inner.clients.get(&link.client).map(|c| c.tx.clone())
        };

        use crate::sip::message::Method;
        let from_header = format!("<sip:brew@{}>;tag={}", self.transport.advertised_host, uuid::Uuid::new_v4().simple());
        let to_header = format!("<{}>", extract_uri(&target_uri));
        let mut invite = SipMessage::new_request(Method::Invite, target_uri.clone());
        invite.push_header("Via", format!("SIP/2.0/UDP {};branch=z9hG4bK{}",
            self.transport.advertised_host, uuid::Uuid::new_v4().simple()));
        invite.push_header("Max-Forwards", "70");
        invite.push_header("From", from_header.clone());
        invite.push_header("To", to_header.clone());
        invite.push_header("Call-ID", call_id.to_string());
        invite.push_header("CSeq", "1 INVITE");
        invite.push_header("Contact", format!("<sip:brew@{}>", self.transport.advertised_host));
        invite.push_header("Content-Type", "application/sdp");
        let offered_payloads = [0u8, 8, 101];
        invite.body = Sdp::build(&self.transport.advertised_host, leg.local_port, &offered_payloads);
        // Best-effort BYE target if the Brew side hangs up first: no remote
        // to-tag is tracked (see DialogBye's note), so `to_header` is reused
        // as-is rather than being a fully RFC 3261-correct dialog match.
        let bye = Some(DialogBye {
            request_uri: target_uri.clone(),
            target_addr,
            from: from_header,
            to: to_header,
        });

        // We don't yet know which of PCMU/PCMA the far end will answer with
        // (the 200 OK isn't correlated back into media setup here — see the
        // "optimistic answer" note above), so run the transcoder at PCMU (0),
        // the payload type we listed first and most gateways default to.
        let payload_type = 0u8;
        let task = crate::transcode::task::spawn(
            leg, payload_type, link.call_id, virtual_rx,
            brew_target_tx.into_iter().collect(),
        );
        self.legs.write().await.insert(call_id.to_string(), BridgedLeg {
            virtual_client, brew_call_id: link.call_id, group: None, task, bye,
        });

        self.transport.send_to(&invite, target_addr).await;
        info!(%call_id, uri = %target_uri, user = ?uri_user(&target_uri), "Brew->SIP INVITE sent (media: ACELP<->G.711 transcoder active)");
    }
}

fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
}

#[allow(unused_imports)]
use RtpRelay as _RtpRelayInUse; // keep the media import meaningful across cfgs
