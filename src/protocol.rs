use std::fmt;
use uuid::Uuid;

/// Brew protocol version implemented by this server, advertised to and expected
/// from clients via the `X-Brew-Version` HTTP header during discovery. See the
/// TETRA Homebrew Protocol specification, "Endpoint, authentication".
pub const BREW_PROTOCOL_VERSION: u8 = 1;

pub const CLASS_SUBSCRIBER: u8 = 0xf0;
pub const CLASS_CALL_CONTROL: u8 = 0xf1;
pub const CLASS_FRAME: u8 = 0xf2;
pub const CLASS_ERROR: u8 = 0xf3;
pub const CLASS_SERVICE: u8 = 0xf4;

pub const SUB_DEREGISTER: u8 = 0;
pub const SUB_REGISTER: u8 = 1;
pub const SUB_REREGISTER: u8 = 2;
pub const SUB_AFFILIATE: u8 = 8;
pub const SUB_DEAFFILIATE: u8 = 9;

pub const CALL_GROUP_TX: u8 = 2;
pub const CALL_GROUP_IDLE: u8 = 3;
pub const CALL_SETUP_REQUEST: u8 = 4;
pub const CALL_SETUP_ACCEPT: u8 = 5;
pub const CALL_SETUP_REJECT: u8 = 6;
pub const CALL_ALERT: u8 = 7;
pub const CALL_CONNECT_REQUEST: u8 = 8;
pub const CALL_CONNECT_CONFIRM: u8 = 9;
pub const CALL_RELEASE: u8 = 10;
pub const CALL_SHORT_TRANSFER: u8 = 11;
pub const CALL_SIMPLEX_GRANTED: u8 = 12;
pub const CALL_SIMPLEX_IDLE: u8 = 13;

pub const FRAME_TRAFFIC_CHANNEL: u8 = 0;
pub const FRAME_SDS_TRANSFER: u8 = 1;
pub const FRAME_SDS_REPORT: u8 = 2;

#[derive(Debug, Clone)]
pub enum BrewMessage {
    Subscriber(SubscriberMessage),
    CallControl(CallControlMessage),
    Frame(FrameMessage),
    Error(ErrorMessage),
    Service(ServiceMessage),
}

#[derive(Debug, Clone)]
pub struct SubscriberMessage {
    pub msg_type: u8,
    pub issi: u32,
    pub timestamp: u64,
    pub fraction: u32,
    pub groups: Vec<u32>,
}

#[derive(Debug, Clone)]
pub struct GroupTransmission {
    pub source: u32,
    pub destination: u32,
    pub priority: u8,
    pub access: u8,
    pub service: u16,
    /// SS-TPI talking-party mnemonic, decoded from the optional trailing
    /// `mnemonic[34]` field added in Brew protocol version 1. `None` when the
    /// peer speaks version 0 or sends an empty mnemonic.
    pub mnemonic: Option<String>,
}

/// Circuit-mode (individual) call setup payload, `struct BrewCircularCall`.
///
/// Wire layout (all little-endian, packed), from the specification:
///   source(4) destination(4) number[32] priority(1) service(1) mode(1)
///   duplex(1) method(1) communication(1) grant(1) permission(1) timeout(1)
///   ownership(1) queued(1) mnemonic[34]
///
/// The trailing `mnemonic[34]` exists only from protocol version 1 and only on
/// `CALL_STATE_SETUP_REQUEST`. `CALL_STATE_CONNECT_REQUEST` carries the same
/// struct truncated exactly at `offsetof(BrewCircularCall, mnemonic)` — i.e. no
/// mnemonic — in every version.
#[derive(Debug, Clone)]
pub struct CircularCall {
    pub source: u32,
    pub destination: u32,
    pub number: String,
    pub priority: u8,
    pub mnemonic: Option<String>,
}

/// `offsetof(struct BrewCircularCall, mnemonic)`: the 40-byte
/// source/destination/number head plus 11 single-byte fields (priority,
/// service, mode, duplex, method, communication, grant, permission, timeout,
/// ownership, queued). This is exactly the payload length of a
/// `CONNECT_REQUEST` and the pre-mnemonic length of a `SETUP_REQUEST`.
const CIRCULAR_CALL_BASE_LEN: usize = 4 + 4 + 32 + 11;
/// `sizeof(struct BrewGroupTransmission)` without the version-1 mnemonic:
/// source(4) destination(4) priority(1) access(1) service(2).
const GROUP_TX_BASE_LEN: usize = 4 + 4 + 1 + 1 + 2;
/// Size in bytes of the SS-TPI `mnemonic[34]` field.
const MNEMONIC_FIELD_LEN: usize = 34;

/// Negotiated Brew protocol version for a single WebSocket connection.
///
/// Real clients (e.g. Basestation) do **not** carry `X-Brew-Version` on the
/// WebSocket handshake — only, optionally, on the preceding HTTP discovery GET.
/// The version is therefore treated as a per-connection property that starts at
/// `V0` and is promoted to `V1` either by an explicit discovery header or lazily
/// when a message is observed whose length can only belong to a v1 layout (a
/// call-control payload that includes the `mnemonic[34]` tail). Once promoted,
/// a connection never falls back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConnVersion {
    #[default]
    V0,
    V1,
}

impl ConnVersion {
    pub fn as_u8(self) -> u8 {
        match self { ConnVersion::V0 => 0, ConnVersion::V1 => 1 }
    }
    pub fn has_mnemonic(self) -> bool {
        matches!(self, ConnVersion::V1)
    }
    /// Promotes from a discovery `X-Brew-Version` header value, if trustworthy.
    pub fn from_header_value(v: Option<u8>) -> Self {
        match v { Some(x) if x >= 1 => ConnVersion::V1, _ => ConnVersion::V0 }
    }
}

#[derive(Debug, Clone)]
pub enum CallPayload {
    GroupTransmission(GroupTransmission),
    CircularCall(CircularCall),
    Cause(u8),
    Empty,
    ShortTransfer { source: u32, destination: u32 },
    Raw(Vec<u8>),
}

#[derive(Debug, Clone)]
pub struct CallControlMessage {
    pub call_state: u8,
    pub identifier: Uuid,
    pub payload: CallPayload,
}

#[derive(Debug, Clone)]
pub struct FrameMessage {
    pub frame_type: u8,
    pub identifier: Uuid,
    pub length_bits: u16,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct ErrorMessage {
    pub error_type: u8,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct ServiceMessage {
    pub service_type: u8,
    pub json_data: String,
}

#[derive(Debug)]
pub enum ParseError {
    TooShort(usize),
    UnknownClass(u8),
    InvalidUtf8,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooShort(n) => write!(f, "Brew packet too short: {n} bytes"),
            Self::UnknownClass(c) => write!(f, "unknown Brew class 0x{c:02x}"),
            Self::InvalidUtf8 => write!(f, "invalid UTF-8 service payload"),
        }
    }
}

impl std::error::Error for ParseError {}

fn u16le(data: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([data[o], data[o + 1]])
}

fn u32le(data: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([data[o], data[o + 1], data[o + 2], data[o + 3]])
}

fn u64le(data: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(data[o..o + 8].try_into().expect("checked length"))
}

/// Decodes an SS-TPI mnemonic name as carried inside Brew (protocol version 1),
/// following ETSI EN 300 392-9 clause 8.4.2, table 17:
///   - Octet 0: bit 7 = 0, bits 6-0 = text coding scheme
///   - Octet 1: length of the following text in bits
///   - Octet 2+: encoded character data
///
/// Only the coding schemes practically used by Basestation/Basestation are
/// decoded to text: 0x00 (ISO 8859-1 / 8-bit) and the 7-bit GSM-like packing
/// (scheme 0x01) described in ETSI EN 300 392-2 clause 29.5.4. Unknown schemes
/// return the raw bytes rendered as lossy UTF-8 so information is not silently
/// dropped. Returns `None` for an all-zero / empty field.
pub fn decode_mnemonic(field: &[u8]) -> Option<String> {
    if field.len() < 2 {
        return None;
    }
    let scheme = field[0] & 0x7f;
    let len_bits = field[1] as usize;
    if len_bits == 0 {
        return None;
    }
    let text = &field[2..];

    let decoded = match scheme {
        // 8-bit schemes: one octet per character. Coding scheme 0 is the raw
        // 8-bit default; treat it as ISO 8859-1.
        0x00 => {
            let nchars = len_bits / 8;
            let take = nchars.min(text.len());
            text[..take].iter().map(|&b| b as char).collect::<String>()
        }
        // 7-bit packed alphabet (ETSI EN 300 392-2, 29.5.4).
        0x01 => decode_7bit_packed(text, len_bits),
        // Unknown / other coding schemes: best-effort lossy rendering.
        _ => {
            let nbytes = len_bits.div_ceil(8);
            let take = nbytes.min(text.len());
            String::from_utf8_lossy(&text[..take]).into_owned()
        }
    };

    let trimmed = decoded.trim_end_matches(['\0', ' ']).to_string();
    if trimmed.is_empty() { None } else { Some(trimmed) }
}

/// Unpacks a 7-bit packed character string (septets, LSB-first) into text.
fn decode_7bit_packed(data: &[u8], len_bits: usize) -> String {
    let nchars = len_bits / 7;
    let mut out = String::with_capacity(nchars);
    let mut bit_pos = 0usize;
    for _ in 0..nchars {
        let byte_idx = bit_pos / 8;
        let bit_off = bit_pos % 8;
        if byte_idx >= data.len() {
            break;
        }
        let mut value = (data[byte_idx] >> bit_off) as u16;
        if bit_off > 1 && byte_idx + 1 < data.len() {
            value |= (data[byte_idx + 1] as u16) << (8 - bit_off);
        }
        let septet = (value & 0x7f) as u8;
        out.push(septet as char);
        bit_pos += 7;
    }
    out
}

/// Extracts a NUL-terminated ASCII field of fixed width into an owned String.
fn ascii_field(data: &[u8]) -> String {
    let end = data.iter().position(|&b| b == 0).unwrap_or(data.len());
    String::from_utf8_lossy(&data[..end]).into_owned()
}

/// Parses a Brew message using the legacy version-agnostic path. Retained for
/// callers and tests that do not track a connection version; assumes the most
/// capable layout (v1) so an explicit mnemonic tail is still decoded when
/// present. Prefer [`parse_with_version`] on real connections.
pub fn parse(data: &[u8]) -> Result<BrewMessage, ParseError> {
    parse_with_version(data, ConnVersion::V1).map(|(msg, _)| msg)
}

/// Parses a Brew message in the context of a connection's negotiated version and
/// reports the version implied by this message. The returned `ConnVersion` is
/// the max of the input version and any version lazily detected from the
/// message length (mirroring how Basestation resolves the version from message
/// content when no `X-Brew-Version` handshake header is present). Callers should
/// store `max(previous, returned)` as the connection's version.
pub fn parse_with_version(data: &[u8], version: ConnVersion) -> Result<(BrewMessage, ConnVersion), ParseError> {
    if data.len() < 2 {
        return Err(ParseError::TooShort(data.len()));
    }

    match data[0] {
        CLASS_SUBSCRIBER => parse_subscriber(data).map(|m| (m, version)),
        CLASS_CALL_CONTROL => parse_call_control(data, version),
        CLASS_FRAME => parse_frame(data).map(|m| (m, version)),
        CLASS_ERROR => Ok((BrewMessage::Error(ErrorMessage {
            error_type: data[1],
            data: data[2..].to_vec(),
        }), version)),
        CLASS_SERVICE => parse_service(data).map(|m| (m, version)),
        c => Err(ParseError::UnknownClass(c)),
    }
}

fn parse_subscriber(data: &[u8]) -> Result<BrewMessage, ParseError> {
    if data.len() < 18 {
        return Err(ParseError::TooShort(data.len()));
    }

    let mut groups = Vec::new();
    let mut o = 18;
    while o + 4 <= data.len() {
        groups.push(u32le(data, o));
        o += 4;
    }

    Ok(BrewMessage::Subscriber(SubscriberMessage {
        msg_type: data[1],
        issi: u32le(data, 2),
        timestamp: u64le(data, 6),
        fraction: u32le(data, 14),
        groups,
    }))
}

fn parse_call_control(data: &[u8], version: ConnVersion) -> Result<(BrewMessage, ConnVersion), ParseError> {
    if data.len() < 18 {
        return Err(ParseError::TooShort(data.len()));
    }

    let id = Uuid::from_bytes(data[2..18].try_into().expect("checked length"));
    let payload = &data[18..];
    // Version lazily resolved from this message; promoted to V1 if the payload
    // length can only correspond to a v1 (mnemonic-bearing) layout.
    let mut detected = version;

    let parsed_payload = match data[1] {
        CALL_GROUP_TX => {
            if payload.len() < GROUP_TX_BASE_LEN {
                return Err(ParseError::TooShort(data.len()));
            }
            // A GROUP_TX whose payload extends past the fixed head carries the
            // v1 mnemonic[34]. Its presence alone proves the peer speaks v1, so
            // we promote regardless of the previously-assumed version. A v0 peer
            // sends exactly GROUP_TX_BASE_LEN bytes and no mnemonic.
            let has_tail = payload.len() > GROUP_TX_BASE_LEN;
            if has_tail {
                detected = ConnVersion::V1;
            }
            let mnemonic = if has_tail {
                payload.get(GROUP_TX_BASE_LEN..GROUP_TX_BASE_LEN + MNEMONIC_FIELD_LEN)
                    .or(payload.get(GROUP_TX_BASE_LEN..))
                    .and_then(decode_mnemonic)
            } else {
                None
            };
            CallPayload::GroupTransmission(GroupTransmission {
                source: u32le(payload, 0),
                destination: u32le(payload, 4),
                priority: payload[8],
                access: payload[9],
                service: u16le(payload, 10),
                mnemonic,
            })
        }
        CALL_SETUP_REQUEST => {
            if payload.len() < CIRCULAR_CALL_BASE_LEN {
                return Err(ParseError::TooShort(data.len()));
            }
            // SETUP_REQUEST is the only state that carries the mnemonic[34], and
            // only from v1. A payload longer than the pre-mnemonic base means v1.
            let has_tail = payload.len() >= CIRCULAR_CALL_BASE_LEN + MNEMONIC_FIELD_LEN;
            if has_tail {
                detected = ConnVersion::V1;
            }
            let mnemonic = if has_tail {
                payload.get(CIRCULAR_CALL_BASE_LEN..CIRCULAR_CALL_BASE_LEN + MNEMONIC_FIELD_LEN)
                    .and_then(decode_mnemonic)
            } else {
                None
            };
            CallPayload::CircularCall(CircularCall {
                source: u32le(payload, 0),
                destination: u32le(payload, 4),
                number: ascii_field(&payload[8..40]),
                priority: payload[40],
                mnemonic,
            })
        }
        CALL_CONNECT_REQUEST => {
            // CONNECT_REQUEST is the circular struct truncated exactly at the
            // mnemonic offset in every version — never carries a mnemonic.
            if payload.len() < CIRCULAR_CALL_BASE_LEN {
                return Err(ParseError::TooShort(data.len()));
            }
            CallPayload::CircularCall(CircularCall {
                source: u32le(payload, 0),
                destination: u32le(payload, 4),
                number: ascii_field(&payload[8..40]),
                priority: payload[40],
                mnemonic: None,
            })
        }
        CALL_GROUP_IDLE | CALL_SETUP_REJECT | CALL_RELEASE => {
            if payload.is_empty() {
                return Err(ParseError::TooShort(data.len()));
            }
            CallPayload::Cause(payload[0])
        }
        CALL_SETUP_ACCEPT | CALL_ALERT => CallPayload::Empty,
        CALL_SHORT_TRANSFER => {
            if payload.len() < 8 {
                return Err(ParseError::TooShort(data.len()));
            }
            CallPayload::ShortTransfer {
                source: u32le(payload, 0),
                destination: u32le(payload, 4),
            }
        }
        _ => CallPayload::Raw(payload.to_vec()),
    };

    Ok((BrewMessage::CallControl(CallControlMessage {
        call_state: data[1],
        identifier: id,
        payload: parsed_payload,
    }), detected))
}

fn parse_frame(data: &[u8]) -> Result<BrewMessage, ParseError> {
    if data.len() < 20 {
        return Err(ParseError::TooShort(data.len()));
    }

    Ok(BrewMessage::Frame(FrameMessage {
        frame_type: data[1],
        identifier: Uuid::from_bytes(data[2..18].try_into().expect("checked length")),
        length_bits: u16le(data, 18),
        data: data[20..].to_vec(),
    }))
}

fn parse_service(data: &[u8]) -> Result<BrewMessage, ParseError> {
    let raw = &data[2..];
    let end = raw.iter().position(|b| *b == 0).unwrap_or(raw.len());
    let json_data = std::str::from_utf8(&raw[..end])
        .map_err(|_| ParseError::InvalidUtf8)?
        .to_owned();
    Ok(BrewMessage::Service(ServiceMessage {
        service_type: data[1],
        json_data,
    }))
}


pub fn build_call_cause(call_state: u8, id: &Uuid, cause: u8) -> Vec<u8> {
    let mut out = Vec::with_capacity(19);
    out.push(CLASS_CALL_CONTROL);
    out.push(call_state);
    out.extend_from_slice(id.as_bytes());
    out.push(cause);
    out
}

/// Builds a call-control message with no payload: `CALL_SETUP_ACCEPT` and
/// `CALL_ALERT` parse this way already (`CallPayload::Empty`); `CALL_CONNECT_CONFIRM`
/// has no payload defined by this server's parser either (falls through to
/// `CallPayload::Raw` with zero bytes), so an empty body is the correct wire
/// shape for all three. Used by the SIP<->Brew bridge to drive a private
/// call's accept/ring/answer handshake from the server side (there is no
/// Brew client on the SIP leg to have sent one).
pub fn build_call_control_empty(call_state: u8, id: &Uuid) -> Vec<u8> {
    let mut out = Vec::with_capacity(18);
    out.push(CLASS_CALL_CONTROL);
    out.push(call_state);
    out.extend_from_slice(id.as_bytes());
    out
}

pub fn raw_peer_pair(payload: &CallPayload) -> Option<(u32, u32)> {
    let CallPayload::Raw(raw) = payload else { return None };
    if raw.len() < 8 { return None; }
    Some((u32le(raw, 0), u32le(raw, 4)))
}

/// Bit count of one ACELP-coded TETRA speech frame (30ms @ 8kHz), per
/// ETSI EN 300 395-2. Packed big-endian-bit into `ACELP_CODED_FRAME_BYTES`.
pub const ACELP_CODED_FRAME_BITS: u16 = 137;
/// `ceil(ACELP_CODED_FRAME_BITS / 8)`.
pub const ACELP_CODED_FRAME_BYTES: usize = 18;
/// PCM samples per ACELP frame (30ms @ 8kHz).
pub const ACELP_PCM_SAMPLES: usize = 240;

/// Builds a server-originated `CALL_SETUP_REQUEST` toward a registered Brew
/// subscriber, used to originate a call from the SIP side (there is no Brew
/// client on that leg to have sent one). `number`/the trailing single-byte
/// fields are left zeroed: only source/destination/priority are meaningful
/// for this bridge, and `CircularCall` parsing ignores the rest.
pub fn build_circular_call_setup(id: &Uuid, source: u32, destination: u32, priority: u8) -> Vec<u8> {
    let mut out = Vec::with_capacity(2 + 16 + CIRCULAR_CALL_BASE_LEN);
    out.push(CLASS_CALL_CONTROL);
    out.push(CALL_SETUP_REQUEST);
    out.extend_from_slice(id.as_bytes());
    out.extend_from_slice(&source.to_le_bytes());
    out.extend_from_slice(&destination.to_le_bytes());
    out.extend_from_slice(&[0u8; 32]); // number[32]: unused for a SIP-originated call
    out.push(priority);
    out.extend_from_slice(&[0u8; 10]); // service, mode, duplex, method, communication, grant, permission, timeout, ownership, queued
    out
}

/// Builds a server-originated `CALL_GROUP_TX`, used to seize the group floor
/// on behalf of a SIP caller bridged into a Brew group call.
pub fn build_group_tx(id: &Uuid, source: u32, destination: u32, priority: u8) -> Vec<u8> {
    let mut out = Vec::with_capacity(2 + 16 + GROUP_TX_BASE_LEN);
    out.push(CLASS_CALL_CONTROL);
    out.push(CALL_GROUP_TX);
    out.extend_from_slice(id.as_bytes());
    out.extend_from_slice(&source.to_le_bytes());
    out.extend_from_slice(&destination.to_le_bytes());
    out.push(priority);
    out.push(0); // access
    out.extend_from_slice(&0u16.to_le_bytes()); // service
    out
}

/// Builds a `FRAME_TRAFFIC_CHANNEL` carrying one ACELP-coded speech frame.
pub fn build_traffic_frame(id: &Uuid, coded: &[u8; ACELP_CODED_FRAME_BYTES]) -> Vec<u8> {
    let mut out = Vec::with_capacity(20 + ACELP_CODED_FRAME_BYTES);
    out.push(CLASS_FRAME);
    out.push(FRAME_TRAFFIC_CHANNEL);
    out.extend_from_slice(id.as_bytes());
    out.extend_from_slice(&ACELP_CODED_FRAME_BITS.to_le_bytes());
    out.extend_from_slice(coded);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_call_control_empty_round_trips_as_empty_payload() {
        let id = Uuid::new_v4();
        for state in [CALL_SETUP_ACCEPT, CALL_ALERT] {
            let wire = build_call_control_empty(state, &id);
            let BrewMessage::CallControl(cc) = parse(&wire).unwrap() else { panic!() };
            assert_eq!(cc.call_state, state);
            assert_eq!(cc.identifier, id);
            assert!(matches!(cc.payload, CallPayload::Empty));
        }
    }

    #[test]
    fn parses_group_tx() {
        let id = Uuid::new_v4();
        let mut p = vec![CLASS_CALL_CONTROL, CALL_GROUP_TX];
        p.extend_from_slice(id.as_bytes());
        p.extend_from_slice(&1001u32.to_le_bytes());
        p.extend_from_slice(&91u32.to_le_bytes());
        p.push(3);
        p.push(0);
        p.extend_from_slice(&0u16.to_le_bytes());

        let BrewMessage::CallControl(cc) = parse(&p).unwrap() else { panic!() };
        assert_eq!(cc.identifier, id);
        let CallPayload::GroupTransmission(gt) = cc.payload else { panic!() };
        assert_eq!(gt.source, 1001);
        assert_eq!(gt.destination, 91);
    }

    #[test]
    fn v0_group_tx_has_no_mnemonic_and_stays_v0() {
        // A v0 peer sends exactly the base GROUP_TX with no mnemonic tail.
        let id = Uuid::new_v4();
        let mut p = vec![CLASS_CALL_CONTROL, CALL_GROUP_TX];
        p.extend_from_slice(id.as_bytes());
        p.extend_from_slice(&1001u32.to_le_bytes());
        p.extend_from_slice(&91u32.to_le_bytes());
        p.push(3);
        p.push(0);
        p.extend_from_slice(&0u16.to_le_bytes());

        let (msg, detected) = parse_with_version(&p, ConnVersion::V0).unwrap();
        assert_eq!(detected, ConnVersion::V0, "v0-length message must not promote");
        let BrewMessage::CallControl(cc) = msg else { panic!() };
        let CallPayload::GroupTransmission(gt) = cc.payload else { panic!() };
        assert_eq!(gt.mnemonic, None);
    }

    #[test]
    fn v1_group_tx_promotes_connection() {
        // A GROUP_TX carrying the mnemonic tail proves the peer is v1, even when
        // the connection was previously assumed v0.
        let id = Uuid::new_v4();
        let mut p = vec![CLASS_CALL_CONTROL, CALL_GROUP_TX];
        p.extend_from_slice(id.as_bytes());
        p.extend_from_slice(&1001u32.to_le_bytes());
        p.extend_from_slice(&91u32.to_le_bytes());
        p.push(3);
        p.push(0);
        p.extend_from_slice(&0u16.to_le_bytes());
        let mut mnem = vec![0x00u8, 24];
        mnem.extend_from_slice(b"BOB");
        mnem.resize(34, 0);
        p.extend_from_slice(&mnem);

        let (msg, detected) = parse_with_version(&p, ConnVersion::V0).unwrap();
        assert_eq!(detected, ConnVersion::V1, "mnemonic tail must promote to v1");
        let BrewMessage::CallControl(cc) = msg else { panic!() };
        let CallPayload::GroupTransmission(gt) = cc.payload else { panic!() };
        assert_eq!(gt.mnemonic.as_deref(), Some("BOB"));
    }

    #[test]
    fn connect_request_never_has_mnemonic() {
        // CONNECT_REQUEST is truncated at the mnemonic offset in every version.
        let id = Uuid::new_v4();
        let mut p = vec![CLASS_CALL_CONTROL, CALL_CONNECT_REQUEST];
        p.extend_from_slice(id.as_bytes());
        p.extend_from_slice(&5001u32.to_le_bytes());
        p.extend_from_slice(&6002u32.to_le_bytes());
        p.extend_from_slice(&[0u8; 32]);
        p.push(1);
        p.extend_from_slice(&[0u8; 10]);

        let (msg, detected) = parse_with_version(&p, ConnVersion::V1).unwrap();
        assert_eq!(detected, ConnVersion::V1);
        let BrewMessage::CallControl(cc) = msg else { panic!() };
        let CallPayload::CircularCall(c) = cc.payload else { panic!() };
        assert_eq!(c.mnemonic, None);
    }

    #[test]
    fn header_seeded_v1_still_parses_v0_length_messages() {
        // A connection seeded v1 by the discovery header must not choke on a
        // shorter (mnemonic-less) GROUP_TX; it simply yields no mnemonic.
        let id = Uuid::new_v4();
        let mut p = vec![CLASS_CALL_CONTROL, CALL_GROUP_TX];
        p.extend_from_slice(id.as_bytes());
        p.extend_from_slice(&7u32.to_le_bytes());
        p.extend_from_slice(&8u32.to_le_bytes());
        p.push(1);
        p.push(0);
        p.extend_from_slice(&0u16.to_le_bytes());

        let (msg, _detected) = parse_with_version(&p, ConnVersion::V1).unwrap();
        let BrewMessage::CallControl(cc) = msg else { panic!() };
        let CallPayload::GroupTransmission(gt) = cc.payload else { panic!() };
        assert_eq!(gt.mnemonic, None);
    }

    #[test]
    fn parses_traffic_frame() {
        let id = Uuid::new_v4();
        let mut p = vec![CLASS_FRAME, FRAME_TRAFFIC_CHANNEL];
        p.extend_from_slice(id.as_bytes());
        p.extend_from_slice(&274u16.to_le_bytes());
        p.extend_from_slice(&[0x80; 36]);

        let BrewMessage::Frame(frame) = parse(&p).unwrap() else { panic!() };
        assert_eq!(frame.identifier, id);
        assert_eq!(frame.length_bits, 274);
        assert_eq!(frame.data.len(), 36);
    }

    #[test]
    fn decodes_8bit_mnemonic() {
        // scheme 0x00 (8-bit), 5 chars = 40 bits, "HELLO"
        let mut field = vec![0x00, 40];
        field.extend_from_slice(b"HELLO");
        assert_eq!(decode_mnemonic(&field).as_deref(), Some("HELLO"));
    }

    #[test]
    fn empty_mnemonic_is_none() {
        assert_eq!(decode_mnemonic(&[0x00, 0]), None);
        assert_eq!(decode_mnemonic(&[]), None);
        assert_eq!(decode_mnemonic(&[0x00; 34]), None);
    }

    #[test]
    fn parses_group_tx_with_v1_mnemonic() {
        let id = Uuid::new_v4();
        let mut p = vec![CLASS_CALL_CONTROL, CALL_GROUP_TX];
        p.extend_from_slice(id.as_bytes());
        p.extend_from_slice(&1001u32.to_le_bytes());
        p.extend_from_slice(&91u32.to_le_bytes());
        p.push(3);
        p.push(0);
        p.extend_from_slice(&0u16.to_le_bytes());
        // v1 mnemonic[34]: scheme 0, 24 bits = "BOB"
        let mut mnem = vec![0x00u8, 24];
        mnem.extend_from_slice(b"BOB");
        mnem.resize(34, 0);
        p.extend_from_slice(&mnem);

        let BrewMessage::CallControl(cc) = parse(&p).unwrap() else { panic!() };
        let CallPayload::GroupTransmission(gt) = cc.payload else { panic!() };
        assert_eq!(gt.source, 1001);
        assert_eq!(gt.mnemonic.as_deref(), Some("BOB"));
    }

    #[test]
    fn parses_circular_setup_request() {
        let id = Uuid::new_v4();
        let mut p = vec![CLASS_CALL_CONTROL, CALL_SETUP_REQUEST];
        p.extend_from_slice(id.as_bytes());
        p.extend_from_slice(&5001u32.to_le_bytes()); // source
        p.extend_from_slice(&6002u32.to_le_bytes()); // destination
        let mut number = b"12345".to_vec();
        number.resize(32, 0);
        p.extend_from_slice(&number); // number[32]
        p.push(2); // priority
        p.extend_from_slice(&[0u8; 10]); // remaining 10 single-byte fields
        // v1 mnemonic
        let mut mnem = vec![0x00u8, 32];
        mnem.extend_from_slice(b"CTRL");
        mnem.resize(34, 0);
        p.extend_from_slice(&mnem);

        let BrewMessage::CallControl(cc) = parse(&p).unwrap() else { panic!() };
        let CallPayload::CircularCall(c) = cc.payload else { panic!() };
        assert_eq!(c.source, 5001);
        assert_eq!(c.destination, 6002);
        assert_eq!(c.number, "12345");
        assert_eq!(c.priority, 2);
        assert_eq!(c.mnemonic.as_deref(), Some("CTRL"));
    }

    #[test]
    fn parses_connect_request_without_mnemonic() {
        // CONNECT_REQUEST is the circular struct truncated before the mnemonic.
        let id = Uuid::new_v4();
        let mut p = vec![CLASS_CALL_CONTROL, CALL_CONNECT_REQUEST];
        p.extend_from_slice(id.as_bytes());
        p.extend_from_slice(&5001u32.to_le_bytes());
        p.extend_from_slice(&6002u32.to_le_bytes());
        p.extend_from_slice(&[0u8; 32]);
        p.push(1);
        p.extend_from_slice(&[0u8; 10]);

        let BrewMessage::CallControl(cc) = parse(&p).unwrap() else { panic!() };
        let CallPayload::CircularCall(c) = cc.payload else { panic!() };
        assert_eq!(c.source, 5001);
        assert_eq!(c.mnemonic, None);
    }
}
