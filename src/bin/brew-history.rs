//! Standalone reader for the brew-server append-only history log.
//!
//! Usage:
//!   brew-history <path>            # human-readable text (default)
//!   brew-history <path> --json     # one JSON object per line (pipe into jq)
//!   brew-history --json <path>
//!
//! The log format is defined in `src/store.rs`: a sequence of
//!   [u32 little-endian length N][N bytes of bincode(StoredRecord)]
//!
//! NOTE: the record types below MUST mirror the ones in `src/monitor.rs`
//! (CallRecord, SdsRecord) and `src/store.rs` (StoredRecord). bincode decodes by
//! structure, so any field change there must be reflected here.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CallRecord {
    uuid: Uuid,
    kind: String,
    source: u32,
    destination: u32,
    priority: u8,
    started_at_ms: u64,
    ended_at_ms: Option<u64>,
    voice_frames: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SdsRecord {
    uuid: Uuid,
    source: u32,
    destination: u32,
    at_ms: u64,
    reports: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
enum StoredRecord {
    Call(CallRecord),
    Sds(SdsRecord),
    SdsReport { uuid: Uuid },
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut path: Option<String> = None;
    let mut json = false;
    for a in args.by_ref() {
        match a.as_str() {
            "--json" | "-j" => json = true,
            "--help" | "-h" => {
                eprintln!("usage: brew-history <path> [--json]");
                std::process::exit(0);
            }
            other => {
                if path.is_none() {
                    path = Some(other.to_string());
                } else {
                    eprintln!("unexpected argument: {other}");
                    std::process::exit(2);
                }
            }
        }
    }
    let Some(path) = path else {
        eprintln!("usage: brew-history <path> [--json]");
        std::process::exit(2);
    };

    let records = match replay(&path) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error reading {path}: {e}");
            std::process::exit(1);
        }
    };

    let mut calls = 0u64;
    let mut sds = 0u64;
    let mut reports = 0u64;
    for rec in &records {
        match rec {
            StoredRecord::Call(_) => calls += 1,
            StoredRecord::Sds(_) => sds += 1,
            StoredRecord::SdsReport { .. } => reports += 1,
        }
        if json {
            println!("{}", serde_json::to_string(rec).unwrap());
        } else {
            print_text(rec);
        }
    }

    if !json {
        eprintln!(
            "\n{} record(s): {} call(s), {} SDS, {} SDS report(s)",
            records.len(),
            calls,
            sds,
            reports
        );
    }
}

fn print_text(rec: &StoredRecord) {
    match rec {
        StoredRecord::Call(c) => {
            let dur = c
                .ended_at_ms
                .map(|e| format!("{}s", (e.saturating_sub(c.started_at_ms)) / 1000))
                .unwrap_or_else(|| "-".into());
            println!(
                "{}  CALL   {:<7} {} -> {} pri {} {} frames {} [{}]",
                fmt_ts(c.started_at_ms),
                c.kind,
                c.source,
                c.destination,
                c.priority,
                dur,
                c.voice_frames,
                short(&c.uuid),
            );
        }
        StoredRecord::Sds(s) => {
            println!(
                "{}  SDS    {} -> {} reports {} [{}]",
                fmt_ts(s.at_ms),
                s.source,
                s.destination,
                s.reports,
                short(&s.uuid),
            );
        }
        StoredRecord::SdsReport { uuid } => {
            println!("{}  REPORT for [{}]", " ".repeat(19), short(uuid));
        }
    }
}

fn short(u: &Uuid) -> String {
    u.simple().to_string()[..8].to_string()
}

/// Formats a unix-millis timestamp as UTC "YYYY-MM-DD HH:MM:SS" without pulling
/// in a date crate.
fn fmt_ts(ms: u64) -> String {
    let secs = (ms / 1000) as i64;
    let days = secs.div_euclid(86400);
    let tod = secs.rem_euclid(86400);
    let (h, m, s) = (tod / 3600, (tod % 3600) / 60, tod % 60);
    let (y, mo, d) = civil_from_days(days);
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{m:02}:{s:02}")
}

/// Howard Hinnant's days-from-civil, inverted: civil date from days since epoch.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as i64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Reads the append-only log, returning every complete record; a torn trailing
/// record is ignored. Mirrors `store::Store::replay`.
fn replay(path: &str) -> std::io::Result<Vec<StoredRecord>> {
    let buf = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut out = Vec::new();
    let mut pos = 0usize;
    while pos + 4 <= buf.len() {
        let len =
            u32::from_le_bytes([buf[pos], buf[pos + 1], buf[pos + 2], buf[pos + 3]]) as usize;
        let start = pos + 4;
        let end = start + len;
        if end > buf.len() {
            break;
        }
        match bincode::deserialize::<StoredRecord>(&buf[start..end]) {
            Ok(rec) => out.push(rec),
            Err(_) => break,
        }
        pos = end;
    }
    Ok(out)
}
