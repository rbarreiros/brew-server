//! The bidirectional ACELP<->G.711 transcoder task itself: the "codec shim"
//! that `sip::bridge` documents as its attachment point.

use crate::protocol::{self, ACELP_CODED_FRAME_BYTES, ACELP_PCM_SAMPLES, CLASS_FRAME, FRAME_TRAFFIC_CHANNEL};
use crate::sip::media::RtpLeg;
use crate::transcode::acelp::{AcelpDecoder, AcelpEncoder};
use crate::transcode::g711;
use std::collections::VecDeque;
use tokio::sync::mpsc;
use uuid::Uuid;

/// RTP packetization interval for G.711: 20ms @ 8kHz, the near-universal SIP
/// default (vs. ACELP's 30ms/240-sample frame), so the two sides free-run at
/// different frame sizes and are bridged through sample buffers below.
const RTP_SAMPLES_PER_PACKET: usize = 160;

/// Spawns the transcoder for one bridged call. Runs until either side closes
/// (RTP socket error, or `brew_rx` dropped) or the task is aborted (call
/// teardown, tracked the same way as a plain SIP-SIP relay).
///
/// - RTP arriving on `leg` (G.711, `payload_type` 0=PCMU/8=PCMA) is decoded to
///   PCM, buffered into 240-sample ACELP frames, and pushed to every
///   `brew_targets` sender as a `FRAME_TRAFFIC_CHANNEL` Brew packet addressed
///   to `call_id`.
/// - Brew traffic frames for this call arriving on `brew_rx` (fed by a
///   registered virtual client that stands in for the SIP leg as a call
///   participant, so the router delivers voice frames to it like any other
///   peer) are ACELP-decoded, buffered into 160-sample RTP packets, and sent
///   out on `leg`.
/// - Anything else arriving on `brew_rx` (call-control messages: SETUP_ACCEPT,
///   ALERT, CONNECT_REQUEST, CONNECT_CONFIRM, RELEASE, ...) is not audio this
///   task understands, but it is not noise either -- it is routed to the same
///   virtual client for a reason (accept/ring/answer handshake, hangup). It is
///   forwarded verbatim to `control_tx` for the bridge's call-control state
///   machine to act on, rather than silently dropped.
pub fn spawn(
    leg: RtpLeg,
    payload_type: u8,
    call_id: Uuid,
    mut brew_rx: mpsc::UnboundedReceiver<Vec<u8>>,
    brew_targets: Vec<mpsc::UnboundedSender<Vec<u8>>>,
    control_tx: mpsc::UnboundedSender<Vec<u8>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut encoder = AcelpEncoder::new();
        let mut decoder = AcelpDecoder::new();
        let mut pcm_from_sip: VecDeque<i16> = VecDeque::with_capacity(ACELP_PCM_SAMPLES * 2);
        let mut pcm_from_brew: VecDeque<i16> = VecDeque::with_capacity(ACELP_PCM_SAMPLES * 2);
        let mut seq: u16 = 0;
        let mut timestamp: u32 = 0;
        let ssrc: u32 = call_id.as_u128() as u32;
        let mut rtp_buf = [0u8; 2048];

        loop {
            tokio::select! {
                r = leg.recv(&mut rtp_buf) => {
                    let Ok(n) = r else { break };
                    if n <= 12 { continue; } // header-only/garbage datagram
                    for &b in &rtp_buf[12..n] {
                        pcm_from_sip.push_back(decode_sample(payload_type, b));
                    }
                    while pcm_from_sip.len() >= ACELP_PCM_SAMPLES {
                        let mut frame = [0i16; ACELP_PCM_SAMPLES];
                        for slot in frame.iter_mut() {
                            *slot = pcm_from_sip.pop_front().expect("checked len");
                        }
                        let coded = encoder.encode(&frame);
                        let packet = protocol::build_traffic_frame(&call_id, &coded);
                        for tx in &brew_targets {
                            let _ = tx.send(packet.clone());
                        }
                    }
                }
                msg = brew_rx.recv() => {
                    let Some(raw) = msg else { break };
                    if raw.len() < 20 + ACELP_CODED_FRAME_BYTES
                        || raw[0] != CLASS_FRAME
                        || raw[1] != FRAME_TRAFFIC_CHANNEL
                    {
                        let _ = control_tx.send(raw);
                        continue;
                    }
                    let mut coded = [0u8; ACELP_CODED_FRAME_BYTES];
                    coded.copy_from_slice(&raw[20..20 + ACELP_CODED_FRAME_BYTES]);
                    let pcm = decoder.decode(&coded, false);
                    pcm_from_brew.extend(pcm);
                    while pcm_from_brew.len() >= RTP_SAMPLES_PER_PACKET {
                        let mut rtp = Vec::with_capacity(12 + RTP_SAMPLES_PER_PACKET);
                        rtp.push(0x80); // V=2, no padding/extension/CSRC
                        rtp.push(payload_type & 0x7F);
                        rtp.extend_from_slice(&seq.to_be_bytes());
                        rtp.extend_from_slice(&timestamp.to_be_bytes());
                        rtp.extend_from_slice(&ssrc.to_be_bytes());
                        for _ in 0..RTP_SAMPLES_PER_PACKET {
                            let s = pcm_from_brew.pop_front().expect("checked len");
                            rtp.push(encode_sample(payload_type, s));
                        }
                        seq = seq.wrapping_add(1);
                        timestamp = timestamp.wrapping_add(RTP_SAMPLES_PER_PACKET as u32);
                        let _ = leg.send(&rtp).await;
                    }
                }
            }
        }
    })
}

fn decode_sample(payload_type: u8, byte: u8) -> i16 {
    if payload_type == 8 { g711::alaw_decode(byte) } else { g711::ulaw_decode(byte) }
}

fn encode_sample(payload_type: u8, pcm: i16) -> u8 {
    if payload_type == 8 { g711::alaw_encode(pcm) } else { g711::ulaw_encode(pcm) }
}
