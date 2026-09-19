//! Forwards decoded mobile-station LIP positions to APRS-IS.
//!
//! Every ISSI is reported as its own APRS *object* (`;OBJECTNAME*...`) under
//! this server's single APRS-IS login, the same technique real DMR/D-STAR-to-
//! APRS gateways use to relay many radios' positions through one connection --
//! it needs no per-radio callsign/passcode, unlike a plain position report.
//!
//! `router::handle_sds_header`/`handle_sds_transfer` call `report_position`
//! whenever they decode a LIP fix; this module owns the actual network
//! connection (a reconnecting TCP client, decoupled via an unbounded channel
//! so a slow/down APRS-IS server never blocks call/SDS routing) and a
//! per-ISSI rate limiter.

use crate::config::AprsConfig;
use crate::state::AppState;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

/// One decoded position ready to forward, queued by the router.
pub struct PositionReport {
    pub issi: u32,
    pub lat: f64,
    pub lon: f64,
}

/// Queues a position for APRS-IS forwarding. A no-op (silently dropped) when
/// APRS is disabled or the background task has not been started, exactly like
/// sending to a closed channel.
pub fn report_position(state: &Arc<AppState>, issi: u32, lat: f64, lon: f64) {
    let _ = state.aprs_tx.send(PositionReport { issi, lat, lon });
}

/// Spawns the APRS-IS forwarder. Reads `AppState.config.aprs`; when disabled,
/// just drains the queue so `report_position` sends never back up, and does
/// no network I/O at all.
pub async fn run(state: Arc<AppState>, mut rx: mpsc::UnboundedReceiver<PositionReport>) {
    let cfg = state.config.aprs.clone();
    if !cfg.enabled {
        while rx.recv().await.is_some() {}
        return;
    }
    if cfg.callsign.trim().is_empty() || cfg.passcode.trim().is_empty() {
        warn!("aprs: enabled but callsign/passcode not set; positions will not be forwarded");
        while rx.recv().await.is_some() {}
        return;
    }

    let mut last_sent: HashMap<u32, Instant> = HashMap::new();
    let interval = Duration::from_secs(cfg.reconnect_interval_seconds.max(5));

    loop {
        info!(server = %cfg.server, callsign = %cfg.callsign, "aprs: connecting to APRS-IS");
        match connect(&cfg).await {
            Ok(mut stream) => {
                info!(server = %cfg.server, "aprs: connected and logged in");
                if !pump(&cfg, &mut stream, &mut rx, &mut last_sent).await {
                    // Channel closed: the server is shutting down, not a link failure.
                    return;
                }
                warn!(server = %cfg.server, "aprs: link closed; reconnecting");
            }
            Err(e) => {
                warn!(server = %cfg.server, error = %e, "aprs: connection failed");
            }
        }
        tokio::time::sleep(interval).await;
    }
}

async fn connect(cfg: &AprsConfig) -> std::io::Result<TcpStream> {
    let mut stream = TcpStream::connect(&cfg.server).await?;
    let login = format!("user {} pass {} vers brew-server 1.2\r\n", cfg.callsign, cfg.passcode);
    stream.write_all(login.as_bytes()).await?;
    Ok(stream)
}

/// Runs the connected session: forwards queued position reports as APRS
/// object packets, and drains (and logs) anything APRS-IS sends back so a
/// dead TCP connection is detected via a read error/EOF rather than only
/// noticed on the next failed write. Returns `false` if `rx` was closed
/// (shutdown), `true` if the link itself dropped (caller should reconnect).
async fn pump(
    cfg: &AprsConfig,
    stream: &mut TcpStream,
    rx: &mut mpsc::UnboundedReceiver<PositionReport>,
    last_sent: &mut HashMap<u32, Instant>,
) -> bool {
    let mut discard = [0u8; 512];
    let min_interval = Duration::from_secs(cfg.min_report_interval_seconds);
    loop {
        tokio::select! {
            report = rx.recv() => {
                let Some(report) = report else { return false };
                if cfg.min_report_interval_seconds > 0 {
                    if let Some(last) = last_sent.get(&report.issi) {
                        if last.elapsed() < min_interval {
                            continue;
                        }
                    }
                }
                let packet = build_object_packet(cfg, report.issi, report.lat, report.lon);
                debug!(issi = report.issi, %packet, "aprs: sending object report");
                if let Err(e) = stream.write_all(packet.as_bytes()).await {
                    warn!(error = %e, "aprs: write failed");
                    return true;
                }
                last_sent.insert(report.issi, Instant::now());
            }
            n = stream.read(&mut discard) => {
                match n {
                    Ok(0) => { warn!("aprs: server closed connection"); return true; }
                    Ok(_) => {} // server banners/acks; nothing to act on
                    Err(e) => { warn!(error = %e, "aprs: read failed"); return true; }
                }
            }
        }
    }
}

/// Builds one APRS object report line (TNC2/APRS-IS text format), e.g.:
/// `MYCALL>APRS,TCPIP*:;MS90      *011234z3759.50N/02345.84Ej TETRA MS via brew-server`
fn build_object_packet(cfg: &AprsConfig, issi: u32, lat: f64, lon: f64) -> String {
    let name = object_name(&cfg.object_name_prefix, issi);
    let lat_s = crate::position::format_aprs_lat(lat);
    let lon_s = crate::position::format_aprs_lon(lon);
    let ts = aprs_dhm_timestamp();
    format!(
        "{}>APRS,TCPIP*:;{}*{}z{}{}{}{}{}\r\n",
        cfg.callsign, name, ts, lat_s, cfg.symbol_table, lon_s, cfg.symbol_code, cfg.comment,
    )
}

/// Current UTC day-of-month/hour/minute as APRS's 6-digit `DDHHMM` timestamp
/// field, computed from the wall clock without pulling in a datetime crate.
fn aprs_dhm_timestamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let days = (secs / 86400) as i64;
    let time_of_day = secs % 86400;
    let hour = time_of_day / 3600;
    let minute = (time_of_day % 3600) / 60;
    let (_, _, day) = civil_from_days(days);
    format!("{day:02}{hour:02}{minute:02}")
}

/// Howard Hinnant's `civil_from_days`: converts a day count since the Unix
/// epoch to a proleptic-Gregorian (year, month, day). Only `day` is used
/// here, but the whole triple is computed together per the reference
/// algorithm.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

/// APRS object names are exactly 9 characters, space-padded. Built from
/// `prefix` + the ISSI, truncated to fit if the combination overruns.
fn object_name(prefix: &str, issi: u32) -> String {
    let raw = format!("{prefix}{issi}");
    let mut name: String = raw.chars().take(9).collect();
    while name.chars().count() < 9 {
        name.push(' ');
    }
    name
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_name_pads_to_nine_chars() {
        let n = object_name("MS", 90);
        assert_eq!(n.chars().count(), 9);
        assert_eq!(n, "MS90     ");
    }

    #[test]
    fn object_name_truncates_when_too_long() {
        let n = object_name("MOBILESTATION", 1234567);
        assert_eq!(n.chars().count(), 9);
        assert_eq!(n, "MOBILESTA");
    }

    #[test]
    fn build_object_packet_has_expected_shape() {
        let cfg = AprsConfig {
            enabled: true,
            server: "example:14580".into(),
            callsign: "MYCALL-10".into(),
            passcode: "12345".into(),
            symbol_table: '/',
            symbol_code: 'j',
            comment: "TETRA MS".into(),
            object_name_prefix: "MS".into(),
            min_report_interval_seconds: 60,
            reconnect_interval_seconds: 15,
        };
        let packet = build_object_packet(&cfg, 90, 37.9917, 23.7640);
        assert!(packet.starts_with("MYCALL-10>APRS,TCPIP*:;MS90     *"));
        assert!(packet.contains("z3759.50N/02345.84Ej"));
        assert!(packet.ends_with("TETRA MS\r\n"));
    }

    #[test]
    fn civil_from_days_matches_known_epoch_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_783), (2024, 3, 1)); // spot check, leap year boundary
    }
}
