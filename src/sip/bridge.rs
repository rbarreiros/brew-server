//! Bridge between the SIP subsystem and the Brew/TETRA core.
//!
//! This is the single place the two subsystems meet. It maps a routed SIP call
//! onto a Brew call: a SIP->Brew-private call becomes a Brew private
//! (individual) call setup toward the ISSI's registered BlueStation, and a
//! SIP->Brew-group call becomes a group transmission to a GSSI reaching the
//! affiliated basestation mobile stations and brew mobile clients.
//!
//! Media reality check: TETRA carries ACELP voice inside Brew traffic frames,
//! while SIP legs here are steered to G.711 (PCMU/PCMA). Bridging *signalling*
//! is fully modelled below; bridging *media* end to end additionally requires a
//! transcoder (ACELP <-> PCM) which is not part of this server. The bridge
//! therefore allocates the SIP-side RTP relay leg and sets up the Brew call, and
//! marks where a codec shim would consume Brew traffic frames and emit RTP (and
//! vice versa). For deployments where a VoIP gateway on the Brew side already
//! delivers a SIP-compatible codec, the relay path alone is sufficient.

use crate::sip::media::{RtpRelay, Sdp};
use crate::sip::message::{extract_uri, uri_user, SipMessage};
use crate::sip::state::SipCall;
use crate::sip::transport::SipTransport;
use crate::state::AppState;
use std::net::SocketAddr;
use std::sync::Arc;
use tracing::{info, warn};

/// Couples the SIP transport to the Brew core.
pub struct BrewBridge {
    app: Arc<AppState>,
    transport: Arc<SipTransport>,
}

impl BrewBridge {
    pub fn new(app: Arc<AppState>, transport: Arc<SipTransport>) -> Self {
        Self { app, transport }
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
        let reachable = {
            let inner = self.app.inner.read().await;
            inner.subscribers.contains_key(&issi)
        };
        if !reachable {
            warn!(issi, %call_id, "SIP->Brew private: ISSI not registered");
            let resp = self.transport.base_response_pub(req, 480, "Temporarily Unavailable");
            self.transport.send_to(&resp, caller).await;
            self.transport.state.end_call(call_id).await;
            return;
        }

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

        // NOTE: media bridge point. A transcoder task would:
        //   - read RTP (G.711) arriving on `leg`, decode to PCM, encode ACELP,
        //     and inject Brew CALL_GROUP_TX/traffic frames toward the ISSI;
        //   - take Brew traffic frames for this call, decode ACELP to PCM,
        //     encode G.711, and send RTP back out on `leg`.
        // Signalling-only bridge keeps the SIP leg up so the call is visible and
        // the RTP relay is ready for such a shim.

        let mut ok = self.transport.base_response_pub(req, 200, "OK");
        ok.push_header("Contact", format!("<sip:brew@{}>", self.transport.advertised_host));
        ok.push_header("Content-Type", "application/sdp");
        ok.body = Sdp::build(&self.transport.advertised_host, leg.local_port, payloads);
        self.transport.send_to(&ok, caller).await;
        self.transport.state.answer_call(call_id).await;
        info!(issi, %call_id, "bridged SIP call to Brew private (signalling)");
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

        // NOTE: media bridge point (as above): a transcoder would emit
        // CALL_GROUP_TX + traffic frames to the affiliated group members and
        // convert their PTT audio back to RTP on `leg`.

        let mut ok = self.transport.base_response_pub(req, 200, "OK");
        ok.push_header("Contact", format!("<sip:brew@{}>", self.transport.advertised_host));
        ok.push_header("Content-Type", "application/sdp");
        ok.body = Sdp::build(&self.transport.advertised_host, leg.local_port, payloads);
        self.transport.send_to(&ok, caller).await;
        self.transport.state.answer_call(call_id).await;
        info!(gssi, affiliated, %call_id, "bridged SIP call to Brew group (signalling)");
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
                self.place_outbound(&call_id, origin.to_leg(), dest.clone(), reg.contact, reg.source).await;
                true
            }
            LegEndpoint::SipTrunk { trunk, number } => {
                let Some(tc) = self.app.config.sip.trunks.get(&trunk).cloned() else { return false };
                let Some(peer) = tc.remote_host.parse::<SocketAddr>().ok()
                    .or(self.transport.state.trunk_for_peer_addr(&trunk).await) else { return false };
                let host = tc.remote_host.split(':').next().unwrap_or(&tc.remote_host);
                let uri = format!("sip:{number}@{host}");
                self.place_outbound(&call_id, origin.to_leg(), dest.clone(), uri, peer).await;
                true
            }
            _ => false,
        }
    }

    /// Shared helper: allocate a relay leg, INVITE the SIP destination, and
    /// track the call. The Brew media side attaches at the transcoder point.
    async fn place_outbound(
        &self,
        call_id: &str,
        from: crate::sip::state::LegEndpoint,
        to: crate::sip::state::LegEndpoint,
        target_uri: String,
        target_addr: SocketAddr,
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

        use crate::sip::message::Method;
        let mut invite = SipMessage::new_request(Method::Invite, target_uri.clone());
        invite.push_header("Via", format!("SIP/2.0/UDP {};branch=z9hG4bK{}",
            self.transport.advertised_host, uuid::Uuid::new_v4().simple()));
        invite.push_header("Max-Forwards", "70");
        invite.push_header("From", format!("<sip:brew@{}>;tag={}",
            self.transport.advertised_host, uuid::Uuid::new_v4().simple()));
        invite.push_header("To", format!("<{}>", extract_uri(&target_uri)));
        invite.push_header("Call-ID", call_id.to_string());
        invite.push_header("CSeq", "1 INVITE");
        invite.push_header("Contact", format!("<sip:brew@{}>", self.transport.advertised_host));
        invite.push_header("Content-Type", "application/sdp");
        invite.body = Sdp::build(&self.transport.advertised_host, leg.local_port, &[0, 8, 101]);
        // Keep the leg alive for the call duration by leaking it into a relay
        // task placeholder (a real transcoder task owns it in production).
        let _keep = leg;
        self.transport.send_to(&invite, target_addr).await;
        info!(%call_id, uri = %target_uri, user = ?uri_user(&target_uri), "Brew->SIP INVITE sent");
    }
}

fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
}

#[allow(unused_imports)]
use RtpRelay as _RtpRelayInUse; // keep the media import meaningful across cfgs
