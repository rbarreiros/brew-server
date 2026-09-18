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
                    for &b in &rtp_buf[offset..n] {
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
                    if raw.len() >= 21 && raw[0] == CLASS_FRAME && raw[1] == protocol::FRAME_DTMF {
                        if let Some(event) = dtmf_ascii_to_event(raw[20]) {
                            send_dtmf_event(&leg, event, seq, timestamp, ssrc).await;
                            seq = seq.wrapping_add(3); // 3 RTP packets sent per event, see send_dtmf_event
                        }
                        continue;
                    }
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
