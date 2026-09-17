//! SIP subsystem for brew-server.
//!
//! Adds SIP support alongside the Brew/TETRA core: SIP extensions (user/pass
//! registrations) and SIP trunks (peer VoIP gateways such as Asterisk) can
//! connect, and voice routes bridge calls between SIP extensions, SIP trunks,
//! and Brew endpoints (private ISSIs, groups) that reach brew mobile clients and
//! basestation mobile stations.
//!
//! Module layout:
//! - `message`   : SIP message parse/build.
//! - `auth`      : SIP digest authentication (UAS + UAC).
//! - `media`     : SDP parse/build and the UDP RTP relay.
//! - `routing`   : the voice route table resolver.
//! - `state`     : runtime registrations/trunks/calls (dashboard read model).
//! - `transport` : the UDP SIP server, registrar, B2BUA and trunk client.
//! - `bridge`    : the SIP <-> Brew coupling.

pub mod auth;
pub mod bridge;
pub mod media;
pub mod message;
pub mod routing;
pub mod state;
pub mod transport;

pub use state::{SipSnapshot, SipState};
pub use transport::SipTransport;

/// Runs the SIP subsystem. No-op (returns Ok) when SIP is disabled in config.
pub async fn run(app: std::sync::Arc<crate::state::AppState>) -> anyhow::Result<()> {
    transport::run(app).await
}
