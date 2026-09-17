//! ACELP <-> G.711 media transcoder for the SIP<->Brew bridge.
//!
//! `acelp` wraps the vendored ETSI EN 300 395-2 reference codec (the TETRA
//! side), `g711` implements the PCMU/PCMA companding (the SIP side), and
//! `task` is the bidirectional pump between an RTP leg and a Brew call.

pub mod acelp;
pub mod g711;
pub mod task;
