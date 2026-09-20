//! The bidirectional ACELP<->G.711 transcoder task itself: the "codec shim"
//! that `sip::bridge` documents as its attachment point.

use crate::protocol::{self, ACELP_PCM_SAMPLES, CLASS_FRAME, FRAME_TRAFFIC_CHANNEL, STE_VOICE_PAYLOAD_BYTES};
use crate::sip::media::RtpLeg;
use crate::transcode::acelp::{AcelpDecoder, AcelpEncoder};
use crate::transcode::g711;
use std::collections::VecDeque;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::info;
use uuid::Uuid;

/// RTP packetization interval for G.711: 20ms @ 8kHz, the near-universal SIP
/// default (vs. ACELP's 30ms/240-sample frame), so the two sides free-run at
/// different frame sizes and are bridged through sample buffers below.
const RTP_SAMPLES_PER_PACKET: usize = 160;
const RTP_INTERVAL: Duration = Duration::from_millis(20);
/// One `FRAME_TRAFFIC_CHANNEL` message carries two 30ms ACELP subframes (see
/// `protocol::STE_VOICE_PAYLOAD_BYTES`), so it is emitted every 60ms, not
/// 30ms -- two PCM samples worth of `ACELP_PCM_SAMPLES` (480 total) per tick.
const ACELP_INTERVAL: Duration = Duration::from_millis(60);
const STATS_INTERVAL: Duration = Duration::from_secs(5);

/// Running counts for the periodic stats log below -- the only way to tell,
/// from a real deployment's logs alone, whether a "garbled"/"choppy"/"silent"
/// report is packet loss upstream (rtp_in/acelp_in stop incrementing),
/// pipeline starvation (ticks fire but the buffer is short, undercounting
/// *_out relative to *_in), or something past this task entirely (both
/// directions' counts look healthy, so the bug is elsewhere in the bridge).
#[derive(Default)]
struct Stats {
    rtp_in: u32,
    rtp_out: u32,
    rtp_underflow: u32,
    acelp_in: u32,
    acelp_out: u32,
    acelp_underflow: u32,
}

/// Spawns the transcoder for one bridged call. Runs until either side closes
/// (RTP socket error, or `brew_rx` dropped) or the task is aborted (call
/// teardown, tracked the same way as a plain SIP-SIP relay).
///
/// - RTP arriving on `leg` (G.711, `payload_type` 0=PCMU/8=PCMA) is decoded to
///   PCM and buffered; a 60ms ticker (see below) drains it into STE-packed
///   2-subframe ACELP traffic frames (`protocol::STE_VOICE_PAYLOAD_BYTES`).
/// - Brew traffic frames for this call arriving on `brew_rx` (fed by a
///   registered virtual client that stands in for the SIP leg as a call
///   participant, so the router delivers voice frames to it like any other
///   peer) are ACELP-decoded (both subframes of each STE payload) and
///   buffered; a 20ms ticker drains it into RTP packets sent out on `leg`.
/// - Anything else arriving on `brew_rx` (call-control messages: SETUP_ACCEPT,
///   ALERT, CONNECT_REQUEST, CONNECT_CONFIRM, RELEASE, ...) is not audio this
///   task understands, but it is not noise either -- it is routed to the same
///   virtual client for a reason (accept/ring/answer handshake, hangup). It is
///   forwarded verbatim to `control_tx` for the bridge's call-control state
///   machine to act on, rather than silently dropped.
///
/// Emission is deliberately decoupled from arrival via two `interval` tickers
/// (one 20ms RTP tick, one 60ms ACELP/STE tick) rather than draining each
/// buffer to completion the instant enough samples land. `leg`/`brew_rx` are
/// both bursty: a 60ms STE unit (480 samples) and RTP's 160-sample packet
/// don't share a common multiple shorter than 480 samples, so on a naive
/// drain-on-arrival loop some output units would complete back-to-back with
/// no real-time gap between them -- confirmed live via a production tcpdump
/// capture, which showed this bridge's outbound RTP frequently going out in
/// ~60us-apart pairs instead of a steady 20ms cadence. G.711/SIP jitter
/// buffers mostly tolerate that; a real TETRA Basestation's downlink traffic
/// channel is
/// locked to a rigid TDMA slot schedule and does not -- frames delivered off
/// that cadence are exactly the kind of thing that shows up as garbled or
/// entirely dropped audio on the radio side.
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
        let mut rtp_ticker = tokio::time::interval(RTP_INTERVAL);
        rtp_ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut acelp_ticker = tokio::time::interval(ACELP_INTERVAL);
        acelp_ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut stats_ticker = tokio::time::interval(STATS_INTERVAL);
        let mut stats = Stats::default();

        loop {
            tokio::select! {
                r = leg.recv(&mut rtp_buf) => {
                    let Ok(n) = r else { break };
                    if n <= 12 { continue; } // header-only/garbage datagram
                    // RTP header is 12 bytes plus an optional CSRC list (RFC 3550
                    // 5.1) and an optional extension (X bit); skip both so we
                    // don't decode header bytes as if they were G.711 payload.
                    let cc = (rtp_buf[0] & 0x0f) as usize;
                    let has_ext = rtp_buf[0] & 0x10 != 0;
                    let pt = rtp_buf[1] & 0x7f;
                    let mut offset = 12 + cc * 4;
                    if has_ext {
                        if offset + 4 > n {
                            continue;
                        }
                        let ext_words = u16::from_be_bytes([rtp_buf[offset + 2], rtp_buf[offset + 3]]) as usize;
                        offset += 4 + ext_words * 4;
                    }
                    if offset > n {
                        continue;
                    }
                    // Only decode packets at the negotiated audio payload type.
                    // Anything else sharing this port (comfort noise, RFC 2833
                    // DTMF events, ...) is not G.711 and must not be fed to the
                    // decoder, or it corrupts the PCM stream with garbage
                    // samples -- heard on the ISSI side as garbled audio.
                    if pt != payload_type {
                        continue;
                    }
                    stats.rtp_in += 1;
                    for &b in &rtp_buf[offset..n] {
                        pcm_from_sip.push_back(decode_sample(payload_type, b));
                    }
                }
                msg = brew_rx.recv() => {
                    let Some(raw) = msg else { break };
                    if raw.len() >= 21 && raw[0] == CLASS_FRAME && raw[1] == protocol::FRAME_DTMF {
                        if let Some(event) = dtmf_ascii_to_event(raw[20]) {
                            send_dtmf_event(&leg, event, seq, timestamp, ssrc).await;
                            seq = seq.wrapping_add(3); // 3 RTP packets sent per event, see send_dtmf_event
                        }
                        continue;
                    }
                    if raw.len() < 20 + STE_VOICE_PAYLOAD_BYTES
                        || raw[0] != CLASS_FRAME
                        || raw[1] != FRAME_TRAFFIC_CHANNEL
                    {
                        let _ = control_tx.send(raw);
                        continue;
                    }
                    stats.acelp_in += 1;
                    let payload: &[u8; STE_VOICE_PAYLOAD_BYTES] = raw[20..20 + STE_VOICE_PAYLOAD_BYTES]
                        .try_into().expect("checked len");
                    let (sub1, sub2) = protocol::unpack_ste_voice_payload(payload);
                    pcm_from_brew.extend(decoder.decode(&sub1, false));
                    pcm_from_brew.extend(decoder.decode(&sub2, false));
                }
                // Paced ACELP emission: one 2-subframe (60ms) STE message per
                // tick, matching TETRA's rigid TDMA traffic-channel cadence --
                // never more than one per tick, however many samples piled up
                // between ticks.
                _ = acelp_ticker.tick() => {
                    if pcm_from_sip.len() >= 2 * ACELP_PCM_SAMPLES {
                        let mut frame1 = [0i16; ACELP_PCM_SAMPLES];
                        let mut frame2 = [0i16; ACELP_PCM_SAMPLES];
                        for slot in frame1.iter_mut() { *slot = pcm_from_sip.pop_front().expect("checked len"); }
                        for slot in frame2.iter_mut() { *slot = pcm_from_sip.pop_front().expect("checked len"); }
                        let sub1 = encoder.encode(&frame1);
                        let sub2 = encoder.encode(&frame2);
                        let packet = protocol::build_traffic_frame(&call_id, &sub1, &sub2);
                        for tx in &brew_targets {
                            let _ = tx.send(packet.clone());
                        }
                        stats.acelp_out += 1;
                    } else {
                        stats.acelp_underflow += 1;
                    }
                }
                // Paced RTP emission: one packet every 20ms, matching G.711's
                // standard SIP ptime -- never more than one per tick.
                _ = rtp_ticker.tick() => {
                    if pcm_from_brew.len() >= RTP_SAMPLES_PER_PACKET {
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
                        stats.rtp_out += 1;
                        let _ = leg.send(&rtp).await;
                    } else {
                        stats.rtp_underflow += 1;
                    }
                }
                _ = stats_ticker.tick() => {
                    info!(
                        %call_id,
                        rtp_in = stats.rtp_in, rtp_out = stats.rtp_out, rtp_underflow = stats.rtp_underflow,
                        acelp_in = stats.acelp_in, acelp_out = stats.acelp_out, acelp_underflow = stats.acelp_underflow,
                        pcm_from_sip_buffered = pcm_from_sip.len(), pcm_from_brew_buffered = pcm_from_brew.len(),
                        "transcoder stats (last {STATS_INTERVAL:?}): rtp_in/out are the SIP/PSTN<->us leg, acelp_in/out are the Brew/ISSI<->us leg; *_underflow counts a tick that fired with too little buffered audio to emit (starvation, not corruption)",
                    );
                    stats = Stats::default();
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

/// RFC 4733 (formerly 2833) telephone-event payload type. This codebase's SDP
/// builder (`sip::media::Sdp::build`) always offers/answers 101 for it
/// whenever telephone-event is in the payload list at all, so it is safe to
/// hardcode here rather than threading a negotiated value through.
const TELEPHONE_EVENT_PT: u8 = 101;

/// Maps one Brew `FRAME_DTMF` digit byte to its RFC 4733 event code
/// (0-9, 10='*', 11='#', 12-15='A'-'D'). `None` for anything else.
fn dtmf_ascii_to_event(digit: u8) -> Option<u8> {
    match digit {
        b'0'..=b'9' => Some(digit - b'0'),
        b'*' => Some(10),
        b'#' => Some(11),
        b'A'..=b'D' => Some(12 + (digit - b'A')),
        b'a'..=b'd' => Some(12 + (digit - b'a')),
        _ => None,
    }
}

/// Sends one DTMF tone as RFC 4733 telephone-event RTP packets: the Brew
/// side gives us one already-decoded digit per frame (no press/release
/// timing), so this sends a short, fixed-duration event -- a start packet
/// (marker bit set, per RFC 3551 §4.1) and an end packet (the "E" bit set),
/// both sharing the same RTP timestamp (the event's start), per RFC 4733
/// §2.5.1.4: only the marker bit and end bit distinguish them, and retransmitting
/// the end packet a couple of times guards against a single lost UDP
/// datagram swallowing the whole digit. `start_seq`/`start_seq+1`/`start_seq+2`
/// are consumed (3 packets); the caller advances its running sequence
/// counter by 3 to keep sharing the one RTP sequence space with the audio
/// packets sent on this same leg/SSRC.
async fn send_dtmf_event(leg: &RtpLeg, event: u8, start_seq: u16, ts: u32, ssrc: u32) {
    const DURATION: u16 = 1600; // 200ms @ 8kHz, a typical single DTMF press
    let volume: u8 = 10; // -10 dBm0, a common default; Brew carries no level info to derive this from
    let build = |seq: u16, end: bool| {
        let mut pkt = Vec::with_capacity(16);
        pkt.push(0x80); // V=2, no padding/extension/CSRC
        let marker = if seq == start_seq { 0x80 } else { 0x00 }; // set on the first packet only
        pkt.push(marker | (TELEPHONE_EVENT_PT & 0x7F));
        pkt.extend_from_slice(&seq.to_be_bytes());
        pkt.extend_from_slice(&ts.to_be_bytes());
        pkt.extend_from_slice(&ssrc.to_be_bytes());
        pkt.push(event);
        pkt.push((if end { 0x80u8 } else { 0u8 }) | volume);
        pkt.extend_from_slice(&DURATION.to_be_bytes());
        pkt
    };
    let _ = leg.send(&build(start_seq, false)).await;
    let _ = leg.send(&build(start_seq.wrapping_add(1), true)).await;
    let _ = leg.send(&build(start_seq.wrapping_add(2), true)).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sip::media::RtpRelay;

    /// Reproduces the production symptom this pacing rewrite fixes: feed the
    /// transcoder several Brew ACELP frames all at once (as they'd actually
    /// arrive after any network burst/scheduling jitter) and confirm the
    /// resulting RTP packets go out roughly 20ms apart, never back-to-back --
    /// a live tcpdump capture had previously shown ~60us gaps between pairs of
    /// outbound RTP packets instead of a steady cadence, which is fatal to a
    /// real TETRA Basestation's rigid TDMA-scheduled downlink.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn rtp_output_is_paced_even_when_acelp_frames_arrive_in_a_burst() {
        let relay = RtpRelay::new("127.0.0.1", 44000, 44020);
        let leg = relay.alloc_leg().await.unwrap();
        let listener = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        leg.set_remote(listener.local_addr().unwrap()).await;

        let (brew_tx, brew_rx) = mpsc::unbounded_channel();
        let (control_tx, _control_rx) = mpsc::unbounded_channel();
        let call_id = Uuid::new_v4();
        let handle = spawn(leg, 0 /* PCMU */, call_id, brew_rx, Vec::new(), control_tx);

        // Encode 2 STE messages (2 subframes/480 samples each = 960 samples
        // total, enough for 6 RTP packets) and post both Brew traffic frames
        // in one burst, back-to-back, with no delay between them -- simulating
        // exactly the arrival pattern a WebSocket receive loop produces when
        // frames queue up.
        let mut encoder = AcelpEncoder::new();
        for _ in 0..2 {
            let pcm = [0i16; ACELP_PCM_SAMPLES];
            let sub1 = encoder.encode(&pcm);
            let sub2 = encoder.encode(&pcm);
            let packet = protocol::build_traffic_frame(&call_id, &sub1, &sub2);
            brew_tx.send(packet).unwrap();
        }

        let mut arrivals = Vec::new();
        let mut buf = [0u8; 2048];
        for _ in 0..6 {
            let (_, _) = tokio::time::timeout(std::time::Duration::from_secs(2), listener.recv_from(&mut buf))
                .await.expect("timed out waiting for a paced RTP packet").unwrap();
            arrivals.push(tokio::time::Instant::now());
        }
        handle.abort();

        for w in arrivals.windows(2) {
            let gap = w[1] - w[0];
            assert!(
                gap >= Duration::from_millis(10),
                "RTP packets must be paced ~20ms apart, not emitted back-to-back; got a {gap:?} gap"
            );
        }
    }

    #[test]
    fn dtmf_ascii_maps_to_rfc4733_events() {
        assert_eq!(dtmf_ascii_to_event(b'0'), Some(0));
        assert_eq!(dtmf_ascii_to_event(b'9'), Some(9));
        assert_eq!(dtmf_ascii_to_event(b'*'), Some(10));
        assert_eq!(dtmf_ascii_to_event(b'#'), Some(11));
        assert_eq!(dtmf_ascii_to_event(b'A'), Some(12));
        assert_eq!(dtmf_ascii_to_event(b'd'), Some(15));
        assert_eq!(dtmf_ascii_to_event(b'x'), None);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn send_dtmf_event_produces_three_correctly_shaped_rtp_packets() {
        let relay = RtpRelay::new("127.0.0.1", 43000, 43010);
        let leg = relay.alloc_leg().await.unwrap();
        let listener = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        leg.set_remote(listener.local_addr().unwrap()).await;

        send_dtmf_event(&leg, 5 /* '5' */, 1000, 0xAAAA_BBBB, 0xCAFEBABE).await;

        let mut got = Vec::new();
        let mut buf = [0u8; 64];
        for _ in 0..3 {
            let (n, _) = tokio::time::timeout(std::time::Duration::from_millis(500), listener.recv_from(&mut buf))
                .await.expect("timed out waiting for DTMF packet").unwrap();
            got.push(buf[..n].to_vec());
        }

        for (i, pkt) in got.iter().enumerate() {
            assert_eq!(pkt.len(), 16, "packet {i}");
            assert_eq!(pkt[0], 0x80, "RTP version byte, packet {i}");
            let marker = pkt[1] & 0x80 != 0;
            assert_eq!(marker, i == 0, "marker bit only on the first packet (packet {i})");
            assert_eq!(pkt[1] & 0x7F, TELEPHONE_EVENT_PT, "payload type, packet {i}");
            let seq = u16::from_be_bytes([pkt[2], pkt[3]]);
            assert_eq!(seq, 1000 + i as u16, "sequence numbers must be consecutive");
            let ts = u32::from_be_bytes([pkt[4], pkt[5], pkt[6], pkt[7]]);
            assert_eq!(ts, 0xAAAA_BBBB, "all 3 packets share the event's start timestamp, packet {i}");
            let ssrc = u32::from_be_bytes([pkt[8], pkt[9], pkt[10], pkt[11]]);
            assert_eq!(ssrc, 0xCAFEBABE, "packet {i}");
            assert_eq!(pkt[12], 5, "event code ('5'), packet {i}");
            let end_bit = pkt[13] & 0x80 != 0;
            assert_eq!(end_bit, i != 0, "end bit on the last 2 packets only, packet {i}");
        }
    }
}
