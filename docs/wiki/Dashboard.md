# Web monitoring dashboard

This build includes a zero-setup live dashboard. It now runs on its **own
listener/port**, separate from the Brew API, configured in `[dashboard]`
(default `0.0.0.0:9003`, enabled by default):

```toml
[dashboard]
enabled = true
listen = "0.0.0.0:9003"
realm = "brew-server-dashboard"

[dashboard.users]
# "admin" = "change-me-dashboard"

[dashboard.tls]
enabled = false
```

- Dashboard: `http://<server>:9003/`
- MS map (linked from the dashboard): `/map` — plots decoded MS positions;
  JSON at `/api/positions`
- **Live connections** (linked from the dashboard): `/connections` — who is
  connected/registered *right now*: Brew connections (Basestations, direct
  Terminal/mobile clients, and federation peer links, with remote address and
  how long they've been connected), registered subscribers (every ISSI in
  `inner.subscribers` — whether registered by a Terminal-mode MS, on its
  behalf by a Basestation, or reachable through a federation peer — tagged
  with which of those it came via), and SIP registrations/trunks. JSON at
  `/api/connections`. This is a live snapshot, distinct from `/registrations`
  (see the log pages below), which is a historical event log.
- Log pages (linked from the dashboard): `/calls` (recent calls, 10/page),
  `/sds` (recent SDS, 10/page), `/telemetry-sds` (telemetry SDS log, 5/page),
  `/registrations` (register/deregister/timeout event log)
- JSON snapshot: `/api/status`
- Live event WebSocket: `/api/live`
- Basestation telemetry snapshot: `/api/telemetry` (empty unless the `[telemetry]`
  listener is enabled)
- Basestation control: `/api/control` (connected station IDs) and
  `/api/control/{id}` (POST a command; empty/404 unless `[control]` is enabled)

All dashboard routes sit behind optional HTTP **Basic** auth: add entries to
`[dashboard.users]` to require a username/password (an empty table leaves it
open). Set `[dashboard.tls]` to serve the dashboard over HTTPS/WSS. The Brew API
listener (`listen`, normally `:9000`) now serves only `/brew` and `/healthz` —
the dashboard is no longer mounted there.

### Position mapping

The `/map` page plots the latest known position of each mobile station, from two
sources, both decoded on the Brew SDS channel (`handle_sds_transfer`) — the SDS
that Basestation relays for delivery, not the lossy telemetry `SdsLog`:

- **Binary TETRA LIP** (ETSI TS 100 392-18) short location reports, SDS protocol
  id `0x0A`. The frame is scanned for the `0x0A` PID and the bit-packed PDU is
  decoded: 2-bit PDU type (0 = short report), 2-bit time-elapsed, 25-bit signed
  longitude (`raw * 360 / 2^25`), 24-bit signed latitude (`raw * 180 / 2^24`).
  Verified against a live beacon `0a 01 0e 62 39 b0 43 9a ff e0 20` → 37.9920 N,
  23.7642 E.
- **Textual beacons** — an APRS string (`4426.12N/02606.55E`), decimal degrees
  (`44.4353, 26.1092`), or a Maidenhead locator (`KN34bk`) — parsed from any
  ASCII in the SDS body.

The decoded fix is stored per subscriber ISSI (attributed via the SDS route's
source ISSI) and served at `/api/positions`. **No Basestation change is
required.** Position-beacon SDS rows are also labelled in the Telemetry SDS Log.

Note: this depends on the SDS (with its LIP payload) being relayed over the Brew
channel to a registered destination. A temporary `debug`-level log
(`SDS_TRANSFER raw frame`) dumps each frame's hex to confirm the payload offset
against live traffic; enable it with `RUST_LOG=brew_server=debug` and remove the
line once positions are confirmed on the map.

### Dashboard authentication and HTTPS

The shipped `brew-server.toml` enables both. Set a real password and point the
TLS block at a certificate/key pair:

```toml
[dashboard]
enabled = true
listen = "0.0.0.0:9003"
realm = "brew-server-dashboard"

[dashboard.users]
"admin" = "change-me-dashboard"

[dashboard.tls]
enabled = true
cert_path = "tls/dashboard-cert.pem"
key_path = "tls/dashboard-key.pem"
```

Generate a self-signed cert for lab use (browsers will warn on the self-signed
CA; use a real cert in production):

```bash
mkdir -p tls
openssl req -x509 -newkey rsa:2048 -nodes -days 365 \
  -keyout tls/dashboard-key.pem -out tls/dashboard-cert.pem \
  -subj "/CN=brew-server-dashboard" \
  -addext "subjectAltName=DNS:localhost,IP:127.0.0.1"
```

With TLS on, reach the dashboard at `https://<server>:9003/` and the live
socket over `wss://`. Because Basic auth transmits credentials as reversible
base64, only enable `[dashboard.users]` together with TLS (or behind a trusted
network); do not run auth over plain HTTP in production. `cert_path` is a PEM
chain (leaf first) and `key_path` the matching PKCS#8/RSA key, same format as
the Brew `[tls]` block.

The dashboard shows connected Basestations, registered subscribers, groups, and
active/live group and private calls with durations and voice-frame counts. The
**Basestations** count reflects only `Basestation`-mode connections (actual
Basestation gateways); a `Terminal`-mode connection (a mobile station
registering directly over the Brew protocol) is not a Basestation and is
excluded from this count — it is counted instead by **Subscribers**. The
**Subscribers** count reflects only `Terminal`-mode registrations (actual
mobile stations); a `Basestation` (Basestation gateway) can also hold a
subscriber registration on a client's behalf, but is not itself an MS and is
excluded from this count.
Recent calls, recent SDS, and the telemetry SDS log have moved to their own
paginated pages, linked from the "Logs" panel (`/calls`, `/sds`,
`/telemetry-sds`). When the Basestation Telemetry channel is enabled, it also
shows per-station health, active calls with **carrier + timeslot**, a small
per-carrier **TS1-TS4 timeslot occupancy grid** (busy vs. available), RF
quality, and a **Registered Subscribers** panel listing which ISSIs are
registered on each station; active emergency alarms appear as a banner — see
[Basestation Telemetry](Basestation-Telemetry.md), which is where the carrier-timeslot data comes
from. A **Mobile Station Registrations** log (`/registrations`, linked from
both the "Logs" panel and the Registered Subscribers panel) lists individual
registration lifecycle events — register, deregister, and timeout-drop —
across all connected Basestations, newest first, so registration churn (a
subscriber repeatedly registering/dropping) is visible over time rather than
only as the current registered set. It also includes register/deregister
events seen directly on the Brew protocol channel (a `Terminal`-mode client
registering/deregistering an ISSI with this server, independent of any
Basestation), tagged with the source `brew` so they're told apart from
Basestation-reported events. This log is a rolling in-memory buffer (last 50
events per source) and is not persisted across restarts. When
Control is enabled, each connected station gets a command panel (Kick
MS, DGNA, live SDS, clear emergency, restart/shutdown) — see
[Basestation Control](Basestation-Control.md). Live per-station state (health, active calls, RF quality,
registrations, the registration event log) is in-memory and resets when the
BTS's telemetry/control connection restarts; calls, SDS, and the Telemetry SDS
Log survive a server restart when `[storage]` is enabled (see
[Persistent history](Persistent-History.md)).
