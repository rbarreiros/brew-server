//! Minimal SIP (RFC 3261) message model: just enough of the protocol to run a
//! registrar, a digest-authenticating UAS/UAC and a back-to-back user agent
//! that bridges to the Brew/TETRA side. This is deliberately hand-rolled to
//! match the rest of this codebase (which hand-rolls the Brew wire protocol)
//! and to avoid a heavyweight external SIP stack.
//!
//! It is *not* a full RFC 3261 implementation. It parses the request/status
//! line, folds headers into a case-insensitive multimap, keeps the body as
//! opaque bytes, and offers typed accessors for the headers the server acts on
//! (Via, From, To, Call-ID, CSeq, Contact, Expires, Authorization, etc.). It
//! is lenient on parse (unknown headers are preserved verbatim) and strict on
//! build (we always emit well-formed messages).

use std::collections::HashMap;
use std::fmt::Write as _;

/// A SIP method. Unknown/extension methods are kept as `Other`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Method {
    Register,
    Invite,
    Ack,
    Bye,
    Cancel,
    Options,
    Info,
    Other(String),
}

impl Method {
    pub fn as_str(&self) -> &str {
        match self {
            Method::Register => "REGISTER",
            Method::Invite => "INVITE",
            Method::Ack => "ACK",
            Method::Bye => "BYE",
            Method::Cancel => "CANCEL",
            Method::Options => "OPTIONS",
            Method::Info => "INFO",
            Method::Other(s) => s,
        }
    }

    pub fn parse(s: &str) -> Method {
        match s.to_ascii_uppercase().as_str() {
            "REGISTER" => Method::Register,
            "INVITE" => Method::Invite,
            "ACK" => Method::Ack,
            "BYE" => Method::Bye,
            "CANCEL" => Method::Cancel,
            "OPTIONS" => Method::Options,
            "INFO" => Method::Info,
            _ => Method::Other(s.to_string()),
        }
    }
}

/// Either a request start line or a status line.
#[derive(Debug, Clone)]
pub enum StartLine {
    Request { method: Method, uri: String },
    Status { code: u16, reason: String },
}

/// A parsed SIP message. Headers preserve insertion order via `order` so we can
/// re-emit multi-valued headers (Via, Route) in the right sequence, while
/// `map` gives O(1) case-insensitive lookup.
#[derive(Debug, Clone)]
pub struct SipMessage {
    pub start: StartLine,
    /// Header name (lowercased) -> ordered list of values.
    map: HashMap<String, Vec<String>>,
    /// Insertion order of (lowercased-name) for faithful re-emit.
    order: Vec<String>,
    pub body: Vec<u8>,
}

impl SipMessage {
    pub fn new_request(method: Method, uri: impl Into<String>) -> Self {
        Self {
            start: StartLine::Request { method, uri: uri.into() },
            map: HashMap::new(),
            order: Vec::new(),
            body: Vec::new(),
        }
    }

    pub fn new_response(code: u16, reason: impl Into<String>) -> Self {
        Self {
            start: StartLine::Status { code, reason: reason.into() },
            map: HashMap::new(),
            order: Vec::new(),
            body: Vec::new(),
        }
    }

    pub fn is_request(&self) -> bool {
        matches!(self.start, StartLine::Request { .. })
    }

    pub fn method(&self) -> Option<&Method> {
        match &self.start {
            StartLine::Request { method, .. } => Some(method),
            _ => None,
        }
    }

    pub fn status_code(&self) -> Option<u16> {
        match &self.start {
            StartLine::Status { code, .. } => Some(*code),
            _ => None,
        }
    }

    /// Appends a header value, preserving order (used for Via stacking).
    pub fn push_header(&mut self, name: &str, value: impl Into<String>) {
        let key = name.to_ascii_lowercase();
        self.order.push(key.clone());
        self.map.entry(key).or_default().push(value.into());
    }

    /// Sets a header to a single value, replacing any existing values.
    pub fn set_header(&mut self, name: &str, value: impl Into<String>) {
        let key = name.to_ascii_lowercase();
        if !self.map.contains_key(&key) {
            self.order.push(key.clone());
        } else {
            // Drop stale ordering entries for this key; we re-add one below.
            self.order.retain(|k| k != &key);
            self.order.push(key.clone());
        }
        self.map.insert(key, vec![value.into()]);
    }

    /// First value of a header, if present.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.map.get(&name.to_ascii_lowercase()).and_then(|v| v.first()).map(String::as_str)
    }

    /// All values of a header, in insertion order.
    pub fn headers(&self, name: &str) -> Vec<&str> {
        self.map.get(&name.to_ascii_lowercase()).map(|v| v.iter().map(String::as_str).collect()).unwrap_or_default()
    }

    /// The topmost Via header value (the one that identifies the immediate
    /// upstream hop and carries the transaction branch).
    pub fn top_via(&self) -> Option<&str> {
        self.header("via")
    }

    /// Call-ID, the dialog/registration identifier.
    pub fn call_id(&self) -> Option<&str> {
        self.header("call-id")
    }

    /// CSeq split into (number, method-string).
    pub fn cseq(&self) -> Option<(u32, String)> {
        let raw = self.header("cseq")?;
        let mut it = raw.split_whitespace();
        let num = it.next()?.parse::<u32>().ok()?;
        let method = it.next().unwrap_or("").to_string();
        Some((num, method))
    }

    /// The `tag` parameter of the From header, if present.
    pub fn from_tag(&self) -> Option<String> {
        self.header("from").and_then(parse_tag)
    }

    /// The `tag` parameter of the To header, if present.
    pub fn to_tag(&self) -> Option<String> {
        self.header("to").and_then(parse_tag)
    }

    /// Expires header as an integer, or the `expires` Contact param.
    pub fn expires(&self) -> Option<u64> {
        if let Some(v) = self.header("expires").and_then(|s| s.trim().parse::<u64>().ok()) {
            return Some(v);
        }
        // Fall back to a Contact ";expires=" parameter.
        let contact = self.header("contact")?;
        for part in contact.split(';') {
            if let Some(rest) = part.trim().strip_prefix("expires=") {
                return rest.trim().parse::<u64>().ok();
            }
        }
        None
    }

    /// Serializes to bytes with a correct Content-Length.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = String::new();
        match &self.start {
            StartLine::Request { method, uri } => {
                let _ = write!(out, "{} {} SIP/2.0\r\n", method.as_str(), uri);
            }
            StartLine::Status { code, reason } => {
                let _ = write!(out, "SIP/2.0 {} {}\r\n", code, reason);
            }
        }
        // Emit headers in insertion order, consuming per-key values in turn so
        // repeated headers (Via) come out in the order they were pushed.
        let mut cursor: HashMap<String, usize> = HashMap::new();
        let mut emitted_cl = false;
        for key in &self.order {
            let idx = cursor.entry(key.clone()).or_insert(0);
            if let Some(values) = self.map.get(key) {
                if let Some(v) = values.get(*idx) {
                    let display = canonical_header_name(key);
                    let _ = write!(out, "{}: {}\r\n", display, v);
                    if key == "content-length" { emitted_cl = true; }
                }
            }
            *idx += 1;
        }
        if !emitted_cl {
            let _ = write!(out, "Content-Length: {}\r\n", self.body.len());
        }
        out.push_str("\r\n");
        let mut bytes = out.into_bytes();
        bytes.extend_from_slice(&self.body);
        bytes
    }

    /// Parses a SIP message from a single UDP datagram. Returns None on a
    /// malformed start line; unknown headers are preserved.
    pub fn parse(data: &[u8]) -> Option<SipMessage> {
        // Split headers from body at the first blank line (CRLFCRLF or LFLF).
        let text = String::from_utf8_lossy(data);
        let (head, body) = split_head_body(&text);
        let mut lines = head.split("\r\n").flat_map(|l| l.split('\n'));
        let start_line = lines.next()?.trim_end();
        let start = parse_start_line(start_line)?;

        let mut msg = SipMessage {
            start,
            map: HashMap::new(),
            order: Vec::new(),
            body: body.as_bytes().to_vec(),
        };

        // Header folding: a line beginning with whitespace continues the prior.
        let mut current: Option<(String, String)> = None;
        for line in lines {
            if line.is_empty() { continue; }
            if line.starts_with(' ') || line.starts_with('\t') {
                if let Some((_, ref mut val)) = current {
                    val.push(' ');
                    val.push_str(line.trim());
                }
                continue;
            }
            if let Some((name, val)) = current.take() {
                push_folded(&mut msg, &name, val);
            }
            if let Some((name, val)) = line.split_once(':') {
                current = Some((name.trim().to_string(), val.trim().to_string()));
            }
        }
        if let Some((name, val)) = current.take() {
            push_folded(&mut msg, &name, val);
        }

        // Respect Content-Length when the datagram carried extra bytes.
        if let Some(cl) = msg.header("content-length").and_then(|s| s.trim().parse::<usize>().ok()) {
            if msg.body.len() > cl { msg.body.truncate(cl); }
        }
        Some(msg)
    }
}

/// Compact-form SIP headers expand to their long names on some peers; fold a
/// couple of the common ones so lookups by long name work.
fn push_folded(msg: &mut SipMessage, name: &str, value: String) {
    let lower = name.to_ascii_lowercase();
    let expanded = match lower.as_str() {
        "v" => "via",
        "f" => "from",
        "t" => "to",
        "i" => "call-id",
        "m" => "contact",
        "c" => "content-type",
        "l" => "content-length",
        "s" => "subject",
        "k" => "supported",
        other => other,
    };
    // Some headers legitimately appear multiple times (Via, Route,
    // Record-Route, Contact); comma-split those into separate values.
    if matches!(expanded, "via" | "route" | "record-route") {
        for part in split_commas_outside_quotes(&value) {
            msg.push_header(expanded, part.trim().to_string());
        }
    } else {
        msg.push_header(expanded, value);
    }
}

fn parse_start_line(line: &str) -> Option<StartLine> {
    if let Some(rest) = line.strip_prefix("SIP/2.0 ") {
        let mut it = rest.splitn(2, ' ');
        let code = it.next()?.parse::<u16>().ok()?;
        let reason = it.next().unwrap_or("").to_string();
        Some(StartLine::Status { code, reason })
    } else {
        // METHOD URI SIP/2.0
        let mut it = line.split_whitespace();
        let method = Method::parse(it.next()?);
        let uri = it.next()?.to_string();
        let ver = it.next().unwrap_or("");
        if !ver.starts_with("SIP/") { return None; }
        Some(StartLine::Request { method, uri })
    }
}

fn split_head_body(text: &str) -> (&str, &str) {
    if let Some(idx) = text.find("\r\n\r\n") {
        (&text[..idx], &text[idx + 4..])
    } else if let Some(idx) = text.find("\n\n") {
        (&text[..idx], &text[idx + 2..])
    } else {
        (text, "")
    }
}

/// Extracts a `tag=` parameter value from a From/To header line.
fn parse_tag(header: &str) -> Option<String> {
    for part in header.split(';') {
        if let Some(rest) = part.trim().strip_prefix("tag=") {
            return Some(rest.trim().to_string());
        }
    }
    None
}

/// Splits on commas that are not inside angle brackets or quotes (for Via /
/// Route folding where each entry may itself contain commas in a quoted param).
fn split_commas_outside_quotes(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut in_quotes = false;
    let mut cur = String::new();
    for c in s.chars() {
        match c {
            '"' => { in_quotes = !in_quotes; cur.push(c); }
            '<' if !in_quotes => { depth += 1; cur.push(c); }
            '>' if !in_quotes => { depth -= 1; cur.push(c); }
            ',' if !in_quotes && depth == 0 => { out.push(std::mem::take(&mut cur)); }
            _ => cur.push(c),
        }
    }
    if !cur.trim().is_empty() { out.push(cur); }
    out
}

/// Best-effort canonical casing for well-known header names on output.
fn canonical_header_name(lower: &str) -> String {
    match lower {
        "call-id" => "Call-ID".to_string(),
        "cseq" => "CSeq".to_string(),
        "www-authenticate" => "WWW-Authenticate".to_string(),
        "content-length" => "Content-Length".to_string(),
        "content-type" => "Content-Type".to_string(),
        "max-forwards" => "Max-Forwards".to_string(),
        "user-agent" => "User-Agent".to_string(),
        "record-route" => "Record-Route".to_string(),
        other => {
            // Title-Case each hyphenated token: "via" -> "Via", "from" -> "From".
            other.split('-').map(|t| {
                let mut ch = t.chars();
                match ch.next() {
                    Some(f) => f.to_ascii_uppercase().to_string() + ch.as_str(),
                    None => String::new(),
                }
            }).collect::<Vec<_>>().join("-")
        }
    }
}

/// Extracts the SIP URI inside angle brackets, or the whole value if there are
/// none. e.g. `"Bob" <sip:bob@host>;tag=x` -> `sip:bob@host`.
pub fn extract_uri(header: &str) -> String {
    if let (Some(a), Some(b)) = (header.find('<'), header.find('>')) {
        if b > a { return header[a + 1..b].to_string(); }
    }
    // No brackets: take up to the first ';' parameter.
    header.split(';').next().unwrap_or(header).trim().to_string()
}

/// Extracts the user part of a `sip:user@host` URI.
pub fn uri_user(uri: &str) -> Option<String> {
    let s = uri.trim_start_matches("sip:").trim_start_matches("sips:");
    let at = s.find('@')?;
    Some(s[..at].to_string())
}

/// Extracts `host:port` (or just host) from a `sip:user@host:port` URI.
pub fn uri_host(uri: &str) -> Option<String> {
    let s = uri.trim_start_matches("sip:").trim_start_matches("sips:");
    let hostport = match s.find('@') {
        Some(at) => &s[at + 1..],
        None => s,
    };
    // Strip any trailing URI parameters (;transport=udp) or headers (?).
    let end = hostport.find([';', '?']).unwrap_or(hostport.len());
    let h = &hostport[..end];
    if h.is_empty() { None } else { Some(h.to_string()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REGISTER: &str = "REGISTER sip:brew.example SIP/2.0\r\n\
Via: SIP/2.0/UDP 10.0.0.5:5060;branch=z9hG4bK-abc\r\n\
Max-Forwards: 70\r\n\
From: \"Alice\" <sip:1001@brew.example>;tag=aaa\r\n\
To: <sip:1001@brew.example>\r\n\
Call-ID: call-123\r\n\
CSeq: 1 REGISTER\r\n\
Contact: <sip:1001@10.0.0.5:5060>\r\n\
Expires: 3600\r\n\
Content-Length: 0\r\n\r\n";

    #[test]
    fn parses_register() {
        let m = SipMessage::parse(REGISTER.as_bytes()).unwrap();
        assert_eq!(m.method(), Some(&Method::Register));
        assert_eq!(m.call_id(), Some("call-123"));
        assert_eq!(m.cseq(), Some((1, "REGISTER".to_string())));
        assert_eq!(m.from_tag().as_deref(), Some("aaa"));
        assert_eq!(m.to_tag(), None);
        assert_eq!(m.expires(), Some(3600));
        let contact = extract_uri(m.header("contact").unwrap());
        assert_eq!(contact, "sip:1001@10.0.0.5:5060");
        assert_eq!(uri_user(&contact).as_deref(), Some("1001"));
        assert_eq!(uri_host(&contact).as_deref(), Some("10.0.0.5:5060"));
    }

    #[test]
    fn roundtrips_response() {
        let mut r = SipMessage::new_response(200, "OK");
        r.push_header("Via", "SIP/2.0/UDP 10.0.0.5:5060;branch=z9hG4bK-abc");
        r.push_header("From", "<sip:1001@brew.example>;tag=aaa");
        r.push_header("To", "<sip:1001@brew.example>;tag=srv");
        r.push_header("Call-ID", "call-123");
        r.push_header("CSeq", "1 REGISTER");
        let bytes = r.to_bytes();
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.starts_with("SIP/2.0 200 OK\r\n"));
        assert!(text.contains("Call-ID: call-123\r\n"));
        assert!(text.contains("Content-Length: 0\r\n"));
    }

    #[test]
    fn stacks_multiple_via() {
        let mut req = SipMessage::new_request(Method::Invite, "sip:2002@brew.example");
        req.push_header("Via", "SIP/2.0/UDP server:5060;branch=z9hG4bK-server");
        req.push_header("Via", "SIP/2.0/UDP client:5060;branch=z9hG4bK-client");
        let out = String::from_utf8(req.to_bytes()).unwrap();
        let server_pos = out.find("branch=z9hG4bK-server").unwrap();
        let client_pos = out.find("branch=z9hG4bK-client").unwrap();
        assert!(server_pos < client_pos, "top Via must be emitted first");
    }

    #[test]
    fn folds_comma_separated_via() {
        let raw = "OPTIONS sip:x SIP/2.0\r\nVia: SIP/2.0/UDP a:5060;branch=z1, SIP/2.0/UDP b:5060;branch=z2\r\nContent-Length: 0\r\n\r\n";
        let m = SipMessage::parse(raw.as_bytes()).unwrap();
        assert_eq!(m.headers("via").len(), 2);
    }
}
