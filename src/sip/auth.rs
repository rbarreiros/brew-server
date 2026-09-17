//! SIP digest authentication (RFC 2617 / RFC 3261 §22), MD5 only. Used both as
//! a UAS (challenging registering extensions and inbound trunks) and as a UAC
//! (answering a 401/407 challenge when we register to an outbound trunk).
//!
//! MD5 is what essentially every SIP peer (Asterisk included) still negotiates
//! by default, and the `md5` crate is already a dependency of this project for
//! Brew HTTP digest, so this adds no new crates.

use std::collections::HashMap;

fn md5_hex(input: &str) -> String {
    format!("{:x}", md5::compute(input.as_bytes()))
}

/// Parses a WWW-Authenticate / Authorization header parameter list into a map
/// with lowercased keys and unquoted values.
pub fn parse_params(header: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let value = header.trim().strip_prefix("Digest ").unwrap_or(header.trim());
    // Split on commas that are not inside quotes.
    let mut in_quotes = false;
    let mut cur = String::new();
    let mut parts = Vec::new();
    for c in value.chars() {
        match c {
            '"' => { in_quotes = !in_quotes; cur.push(c); }
            ',' if !in_quotes => parts.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    if !cur.trim().is_empty() { parts.push(cur); }
    for part in parts {
        if let Some((k, v)) = part.split_once('=') {
            out.insert(k.trim().to_ascii_lowercase(), v.trim().trim_matches('"').to_string());
        }
    }
    out
}

/// Computes the expected digest `response` value for the given credentials and
/// challenge parameters, supporting both plain and `qop=auth` flavours.
pub fn compute_response(
    username: &str,
    password: &str,
    realm: &str,
    method: &str,
    uri: &str,
    nonce: &str,
    qop: Option<&str>,
    nc: Option<&str>,
    cnonce: Option<&str>,
) -> String {
    let ha1 = md5_hex(&format!("{username}:{realm}:{password}"));
    let ha2 = md5_hex(&format!("{method}:{uri}"));
    match (qop, nc, cnonce) {
        (Some(q), Some(nc), Some(cnonce)) if q.contains("auth") => {
            md5_hex(&format!("{ha1}:{nonce}:{nc}:{cnonce}:auth:{ha2}"))
        }
        _ => md5_hex(&format!("{ha1}:{nonce}:{ha2}")),
    }
}

/// UAS side: verifies an Authorization header against a known password.
/// `known_nonce` is the challenge nonce we previously issued; the caller is
/// responsible for nonce lifetime/replay policy.
pub fn verify_authorization(
    auth_header: &str,
    password: &str,
    method: &str,
    known_nonce: &str,
) -> bool {
    let p = parse_params(auth_header);
    let (Some(username), Some(realm), Some(uri), Some(nonce), Some(response)) = (
        p.get("username"), p.get("realm"), p.get("uri"), p.get("nonce"), p.get("response"),
    ) else { return false };
    if nonce != known_nonce { return false; }
    let expected = compute_response(
        username, password, realm, method, uri, nonce,
        p.get("qop").map(String::as_str),
        p.get("nc").map(String::as_str),
        p.get("cnonce").map(String::as_str),
    );
    expected.eq_ignore_ascii_case(response)
}

/// UAC side: builds an Authorization header value answering a challenge, given
/// the challenge parameters parsed from a WWW-Authenticate/Proxy-Authenticate.
pub fn build_authorization(
    challenge: &HashMap<String, String>,
    username: &str,
    password: &str,
    method: &str,
    uri: &str,
    cnonce: &str,
    nc: u32,
) -> String {
    let realm = challenge.get("realm").map(String::as_str).unwrap_or("");
    let nonce = challenge.get("nonce").map(String::as_str).unwrap_or("");
    let opaque = challenge.get("opaque").map(String::as_str);
    let qop = challenge.get("qop").map(String::as_str);
    let nc_str = format!("{nc:08x}");
    let response = compute_response(
        username, password, realm, method, uri, nonce,
        qop, Some(&nc_str), Some(cnonce),
    );
    let mut out = format!(
        "Digest username=\"{username}\", realm=\"{realm}\", nonce=\"{nonce}\", uri=\"{uri}\", response=\"{response}\", algorithm=MD5"
    );
    if let Some(q) = qop {
        if q.contains("auth") {
            out.push_str(&format!(", qop=auth, nc={nc_str}, cnonce=\"{cnonce}\""));
        }
    }
    if let Some(o) = opaque {
        out.push_str(&format!(", opaque=\"{o}\""));
    }
    out
}

/// Builds a WWW-Authenticate challenge value for a UAS 401 response.
pub fn build_challenge(realm: &str, nonce: &str) -> String {
    let opaque = md5_hex(&format!("{realm}:sip"));
    format!("Digest realm=\"{realm}\", nonce=\"{nonce}\", qop=\"auth\", opaque=\"{opaque}\", algorithm=MD5")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uac_answer_verifies_at_uas() {
        // Server challenges, client answers, server verifies — full loop.
        let realm = "brew-server";
        let nonce = "deadbeefcafef00d";
        let password = "s3cret";
        let username = "1001";
        let method = "REGISTER";
        let uri = "sip:brew.example";

        let challenge = parse_params(&build_challenge(realm, nonce));
        let auth = build_authorization(&challenge, username, password, method, uri, "0a0a0a0a", 1);
        assert!(verify_authorization(&auth, password, method, nonce));
        // Wrong password must fail.
        assert!(!verify_authorization(&auth, "wrong", method, nonce));
        // Wrong nonce must fail.
        assert!(!verify_authorization(&auth, password, method, "othernonce"));
    }

    #[test]
    fn parse_params_handles_quoted_commas() {
        let h = "Digest realm=\"a,b\", nonce=\"xyz\", qop=\"auth\"";
        let p = parse_params(h);
        assert_eq!(p.get("realm").unwrap(), "a,b");
        assert_eq!(p.get("nonce").unwrap(), "xyz");
    }
}
