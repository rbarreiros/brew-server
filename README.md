# brew-server

Experimental Rust Brew core for linking two or more MidnightBlue Basestation TETRA base stations.

Reference spec from https://wiki.tetrapack.online/tetra/specifications/brew/

Version 1.3 adds:

- **Basestation locations on the MS map.** New `[bts_locations]` config,
  keyed by the same numeric Brew username each Basestation authenticates
  with under `[auth.users]` -- so an entry automatically matches whichever
  live connection logs in as that identity, with no separate ID scheme.
  Each entry (`name`, `lat`, `lon`) is editable from the `/settings`
  dashboard page (or the raw-TOML editor) and shown as its own marker on
  `/map` (new `/api/bts-locations`, merging the fixed config location with
  live connection state), with a popup showing the Basestation's name,
  coordinates, live IP address and connect status. Threading the
  authenticated username through to the connection required carrying it from
  Digest verification (`server::verify_digest`) through the auth-session
  handshake to `Client.username`, which previously only tracked the
  connection's mode/version/remote address.

Version 1.2 adds:

- **APRS forwarding for mobile-station LIP positions.** New `[aprs]` config
  (`enabled`, `server` — an APRS-IS host:port, `callsign`/`passcode` — this
  server's own APRS-IS login, `symbol_table`/`symbol_code`, `comment`,
  `object_name_prefix`, `min_report_interval_seconds` rate limit,
  `reconnect_interval_seconds`). When enabled, every LIP fix decoded from
  Brew SDS traffic (`router::handle_sds_header`/`handle_sds_transfer`, the
  same decode path that already feeds the dashboard's MS map) is also queued
  to a new `aprs` module, which maintains a reconnecting TCP link to
  APRS-IS and reports each ISSI as its own APRS *object*
  (`;OBJECTNAME*DDHHMMz...`) under this server's single login — the same
  technique real DMR/D-STAR-to-APRS gateways use, so no per-radio APRS
  callsign/passcode is needed. Position queuing is decoupled via a channel so
  a slow or unreachable APRS-IS server never blocks call/SDS routing.
- **Fixed garbled SIP->ISSI audio caused by unfiltered RTP.** The
  transcoder's RTP receive loop decoded every incoming UDP datagram's bytes
  after a fixed 12-byte header as G.711 audio, regardless of the packet's
  actual payload type and without accounting for an optional CSRC list or
  header extension. Anything else sharing the port — comfort noise, RFC 2833
  DTMF events, or a packet with CSRC/extension data — got its bytes decoded
  as if they were audio samples, corrupting the PCM handed to the ACELP
  encoder. Now the payload offset is computed from the real CSRC
  count/extension bit, and any packet whose payload type doesn't match the
  negotiated codec is dropped instead of decoded.

Version 1.1 adds:

- **Server-to-server federation.** Multiple brew-server instances can now be
  linked (chain or star topology) so calls, SDS and subscriber/group
  registrations reach a remote site's Basestations and mobile stations. A
  peer link connects and authenticates exactly like a Basestation does, over
  the same Brew WebSocket protocol, tagged `X-Brew-Mode: Peer` (new
  `[[federation.peers]]` config, dialled outbound with reconnect; an inbound
  link needs no matching config, just Basestation-style auth). Registrations
  propagate peer to peer automatically — each server relays what it learns to
  its *other* peers (split-horizon, safe for any loop-free topology) — so
  private/group call routing and SDS forwarding across servers need no
  federation-specific routing code at all: they already resolve a
  destination via the same `inner.subscribers`/`inner.group_clients` tables
  used for local routing, which now include remote entries. A newly
  (re)connected peer gets a full snapshot of everything this server currently
  knows, in both directions, so it isn't blind to registrations that predate
  the link.
- **DTMF forwarding.** Some real clients (e.g. nexus-bs, a FlowStation-derived
  Basestation) send in-call DTMF as a Brew `FRAME_DTMF` frame (one ASCII
  digit per frame) — outside this server's original protocol coverage, and
  previously silently dropped. It now routes like a voice frame to every
  other Brew-side call participant, and for a SIP-bridged call the
  transcoder converts it to RFC 4733 (formerly 2833) telephone-event RTP
  instead of dropping it there too.
- **Per-ISSI RSSI from the main Brew channel.** Some real clients also send
  `CLASS_SERVICE` type `0x10` (`{"issi":N,"rssi_dbfs":F}`) — previously
  parsed but unconditionally ignored. It's now stored and exposed at
  `/api/rssi`, merged into the dashboard's existing "MS RSSI" column
  alongside the Basestation Telemetry channel's own per-station RSSI.

Version 1.0 adds:

- **ACELP<->G.711 media transcoder for SIP<->Brew calls.** SIP legs are
  steered to G.711 (PCMU/PCMA); Brew traffic frames carry ACELP. A new
  `transcode` module vendors the ETSI EN 300 395-2 reference TETRA codec
  (`third_party/tetra-codec/`, compiled via `build.rs`) alongside a pure-Rust
  G.711 implementation, and a bidirectional pump (`transcode::task`) bridges
  RTP and Brew traffic frames in both directions, so PSTN/SIP calls to and
  from a mobile terminal actually carry audio, not just signalling.
- **Complete Brew<->SIP private-call accept/ring/answer handshake.**
  Previously the bridge answered SIP `INVITE`s with `200 OK` immediately and
  never reacted to the ISSI's `SETUP_ACCEPT`/`ALERT`/`CONNECT_REQUEST` —
  callers got no ringback, and pressing accept on a mobile terminal did
  nothing. Now: `SETUP_ACCEPT`/`ALERT` -> SIP `180 Ringing`; `CONNECT_REQUEST`
  (accept pressed) -> `CALL_CONNECT_CONFIRM` back to the ISSI *and* SIP
  `200 OK` together; `SETUP_REJECT`/`RELEASE` before answer -> SIP `486` and
  teardown. The reverse direction (Brew->SIP) sends `CALL_SETUP_ACCEPT`
  immediately and drives `CALL_ALERT`/`CALL_CONNECT_CONFIRM` from Asterisk's
  own `180`/`200` responses, plus the SIP `ACK` a `200 OK` to our own
  outbound `INVITE` requires (previously missing — Asterisk would keep
  retransmitting the `200` and drop the dialog). Fixed along the way:
  `CALL_CONNECT_CONFIRM` needs a 2-byte grant/permission payload, not an
  empty one (real clients reject it outright otherwise); and the three
  pre-built SIP responses (`180`/`200`/`486`) now share one dialog `To`-tag
  instead of each independently generating its own, which previously caused
  a `BYE` built from the wrong tag to get `481`'d by the peer.
- **Route a mobile terminal's PSTN-style dialled number to SIP.** A terminal
  dialling a non-ISSI number (e.g. "9" + a 10-digit PSTN number) arrives with
  `destination = 0` and the digits in the Brew `CircularCall`'s ASCII
  `number` field, not `destination` — previously ignored entirely. That field
  is now used as the dialled string for `[[sip.routes]]` matching when
  present, and a new `strip_prefix` route field removes a leading literal
  (e.g. the outside-line "9") before it reaches an empty-`number` SIP trunk
  destination.
- **Dashboard settings editor.** A new `/settings` page can add/update/delete
  SIP extensions, trunks and voice routes, plus a raw-TOML editor covering
  every other setting. Saves validate then write atomically to the running
  process's config file, reusing the existing config-watcher restart-to-apply
  mechanism — no new hot-reload path needed.
- **Live connections page.** `/connections` (JSON at `/api/connections`)
  shows who is connected/registered *right now*: Brew connections (mode,
  protocol version, remote address, connect time), registered subscribers
  (both Terminal-mode MS and Basestation-gateway registrations, matching the
  main dashboard's panel), and SIP registrations/trunks. Distinct from
  `/registrations`, which is a historical event log.
- **Max call duration limits.** New `max_call_duration_seconds` (Brew
  station/private/group calls) and `sip.max_call_duration_seconds` (SIP
  calls) config settings force-end a call once it has run too long, the same
  way a normal hangup would (`CALL_RELEASE`/`CALL_GROUP_IDLE` or a SIP `BYE`,
  not a silent kill). Default 4 hours; `0` disables.
- **Server version shown on every dashboard page**, under the live-status
  indicator.
- Renamed `BlueStation`/`FlowStation` references throughout (code, UI, docs)
  to a single consistent `Basestation`/`Basestations`, matching the existing
  `ClientMode::Basestation`. The two WebSocket subprotocol identifiers real
  hardware negotiates with (`bluestation-control-v1`,
  `bluestation-telemetry-v2`) are deliberately left unchanged — they're wire
  compatibility strings, not display text. Also renamed the main dashboard's
  "Logs" panel to "Menu".
- **Fixed a misconfigured `sip.advertised_host` producing malformed SDP.** If
  `advertised_host` is accidentally set to `host:port` instead of a bare
  host (it's written verbatim into the SDP `c=`/`o=` lines, which never
  carry a port), the port is now stripped with a warning instead of silently
  emitting SDP that peers like Asterisk reject.

Version 0.8 adds:

- **Persistent telemetry SDS log.** SDS entries observed on a Basestation
  Telemetry channel (`SdsLog`) are now also appended to the same append-only
  history log used for calls/SDS, tagged with the reporting station, so the
  Telemetry SDS Log survives a server restart instead of resetting with the
  BTS's live in-memory state. Replayed on startup like the rest of `[storage]`
  history, and readable with the same `brew-history` tool (new `SdsTelemetry`
  record type).

Version 0.7 adds:

- **Persistent history.** Completed calls and SDS are written to an append-only
  binary log (`bincode`-framed, crash-safe on read) and replayed on startup, so
  call/SDS history and counters survive restarts. Configured under `[storage]`
  (`enabled`, `path`); it keeps everything with no rotation. A torn trailing
  record from a hard crash is detected and skipped. Read the log with the
  bundled `brew-history` tool: `brew-history brew-history.bin` for readable text,
  or `brew-history brew-history.bin --json` to pipe into `jq`.

- **Position mapping.** SDS position beacons are decoded to latitude/longitude,
  tracked per subscriber ISSI, and plotted on a new `/map` page (Leaflet +
  OpenStreetMap); a `/api/positions` endpoint exposes the latest fixes. Two
  sources are supported: **binary TETRA LIP** short location reports (ETSI TS
  100 392-18), decoded from the raw SDS relayed over the Brew channel, and
  **textual** beacons (APRS, decimal degrees, Maidenhead). No Basestation change
  is required — the LIP payload is decoded in `handle_sds_transfer` from the SDS
  that the Brew channel already relays. See "Position mapping" below.

Version 0.6 adds:

- **Brew protocol version 1 support.** The server advertises and negotiates the
  protocol version via the `X-Brew-Version` header on the discovery GET
  (responding `426 Upgrade Required` for versions it does not implement). Because
  real clients (e.g. Basestation) send no version header on the WebSocket
  handshake, the version is tracked **per connection** and resolved *lazily from
  message content*, defaulting to v0 and promoting to v1 once a v1-shaped
  call-control message is seen. The v1 SS-TPI `mnemonic[34]` talking-party name
  is parsed on `GROUP_TX`/`SETUP_REQUEST` (ETSI EN 300 392-9), and the
  `X-Brew-Mode` header (`Terminal`/`Basestation`) is tracked per client.
- **Dashboard control-panel fix.** The Basestation Control panel no longer wipes
  operator input: it reconciles station cards incrementally instead of rebuilding
  the DOM on every refresh, and reconnects its WebSocket in the background rather
  than reloading the page.
- **Paginated logs.** Recent calls, Recent SDS and the Telemetry SDS Log are
  paginated (10, 10 and 5 rows per page respectively) and have moved off the main
  dashboard onto their own linked pages: `/calls`, `/sds`, and `/telemetry-sds`.
- **Timeslot occupancy graphic.** Each Basestation telemetry card shows a small
  per-carrier TS1-TS4 grid indicating which timeslots are busy vs. available.
- **Registered-subscribers frame.** A dashboard panel lists which subscriber
  ISSIs are registered on each connected Basestation.

Version 0.5 adds:

- The monitoring dashboard now runs on its **own listener/port** (`[dashboard]`,
  default `:9003`), separate from the Brew protocol API. The Brew listener
  (`:9000`) serves only `/brew` and `/healthz`.
- Optional HTTP **Basic** authentication for the dashboard (`[dashboard.users]`).
- Optional native **TLS/HTTPS** for the dashboard (`[dashboard.tls]`), so it can
  be reached over `https://` / `wss://` independently of the Brew `[tls]` block.

Version 0.4 adds:

- Optional Basestation Telemetry ingestion channel (registrations, calls with
  carrier/timeslot, RF/DSP quality, SDR/host health, SDS log, emergency alarms),
  surfaced on the dashboard with an emergency-alarm banner.
- Optional Basestation Control channel (Kick MS, DGNA assign/deassign, live SDS
  add/delete/clear, clear emergency, restart/stop the service), with a per-station
  command panel on the dashboard.

Version 0.3 adds:

- TLS Support for https:// and wss://

Version 0.2 adds:

- HTTP Digest authentication compatible with Basestation's current WebSocket transport (MD5 + qop=auth).
- Single-use authenticated WebSocket session URLs returned by the discovery GET.
- Subscriber registration and talkgroup affiliation routing.
- Group speech routing with priority-based floor pre-emption.
- SDS routing using `SHORT_TRANSFER` + `SDS_TRANSFER`, and reverse `SDS_REPORT` delivery.
- Experimental private/simplex call routing for Brew call states 4..13.

## Important compatibility note

The current Basestation source defines private/simplex state constants, but its Brew parser keeps most of those payloads as raw bytes and its worker currently exposes group voice/SDS commands rather than private-call commands. As of v0.6 this server parses private `SETUP_REQUEST`/`CONNECT_REQUEST` payloads into a structured `BrewCircularCall` (source ISSI, destination ISSI, dialled number, priority, and the v1 `mnemonic`), and routes subsequent control/traffic packets by UUID. For any peer whose payload cannot be fully structured it falls back to the earlier conservative behaviour: the first two little-endian `u32` values are interpreted as source and destination ISSI. Validate this against captures/specification before production use.

## Build and run

```bash
cargo run --release -- brew-server.toml
```

or:

```bash
docker compose up --build
```

Health check:

```bash
curl http://127.0.0.1:9000/healthz
```

## Configuration

`brew-server.toml`:

The configuration file is **watched while the server runs**: when it changes,
the server validates the new file and, if it parses, **restarts the whole
process** (re-executing itself with the same arguments) so the new configuration
takes effect from a clean state — all listeners rebind and in-memory state is
rebuilt. Changes are detected within a couple of seconds. A malformed edit is
logged and ignored (no restart), so a bad edit can't drop the server into a
crash loop. Because the reload is a full process restart, run under a supervisor
(systemd, Docker `restart:` policy, etc.) as normal; active connections are
dropped and clients reconnect.

```toml
listen = "0.0.0.0:9000"
websocket_path = "/brew/"
websocket_subprotocol = "brew"
route_without_affiliations = true
allow_multiple_calls_per_group = true
higher_priority_number_wins = true
preempt_cause = 1
max_call_duration_seconds = 14400 # force-end a Brew call (station or SIP-bridged) past this; 0 disables

[tls]
enabled = false
cert_path = "/etc/brew-server/tls/cert.pem"
key_path = "/etc/brew-server/tls/key.pem"

[auth]
enabled = true
realm = "brew-server"
session_ttl_seconds = 300

[auth.users]
# Brew usernames must be numeric, max 7 digits.
"1000001" = "change-me-bs1"
"1000002" = "change-me-bs2"

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

The `[dashboard]` block controls the monitoring UI on its own port, separate
from the Brew API above — see "Web monitoring dashboard" below for auth and TLS
details. Use a different username/password for each Basestation. The Brew username is an HTTP Digest identity that must be **numeric and at most 7 digits** (a connection presenting a longer or non-numeric username is refused); it does not have to equal a radio ISSI, though a numeric site identity is convenient.

## TLS

Brew can terminate TLS natively so Basestations connect over `wss://` / `https://` without a reverse proxy. Enable it in `[tls]`:

```toml
[tls]
enabled = true
cert_path = "/etc/brew-server/tls/cert.pem"
key_path = "/etc/brew-server/tls/key.pem"
```

`cert_path` is a PEM certificate chain (leaf first, then any intermediates) and `key_path` is the matching PEM private key (PKCS#8 or RSA). When `enabled = true` the listener serves HTTPS on the same `listen` address, so clients, the discovery GET, session WebSockets, and the dashboard all move to `https://`/`wss://`.

Generate a self-signed cert for lab use:

```bash
mkdir -p tls
openssl req -x509 -newkey rsa:2048 -nodes -days 365 \
  -keyout tls/key.pem -out tls/cert.pem -subj "/CN=brew-server"
```

For Docker, mount the certs (the provided `docker-compose.yml` mounts `./tls` to `/etc/brew-server/tls`) and set the paths accordingly.

## Basestation side

Configure each Basestation's Brew transport to point at the server host/port, use endpoint `/brew` (or `/brew/`), subprotocol `brew`, and set the matching Digest username/password. With Digest credentials configured, current Basestation performs:

1. `GET /brew/` without credentials.
2. Server returns `401` with a Digest challenge.
3. Basestation retries with `Authorization: Digest ...`.
4. Server returns a one-time path such as `/brew/session/<token>`.
5. Basestation upgrades that path to WebSocket with subprotocol `brew`.

## Protocol version negotiation

The server implements **Brew protocol version 1**. Clients may advertise the
version they speak with an `X-Brew-Version` header on the discovery `GET`:

- A matching (or lower, still-supported) version is accepted; the server echoes
  `X-Brew-Version` on the `200` response.
- An unsupported version gets `426 Upgrade Required`.
- A **missing** header is accepted for backward compatibility, and the version is
  then determined per connection from the message stream.

Because the WebSocket handshake itself carries no version header, the version is
a **per-connection** property that starts at v0 and is *promoted lazily* to v1
the first time a v1-shaped call-control message (one carrying the `mnemonic[34]`
tail) is observed. This mirrors how Basestation resolves the version and is
logged once per connection (`Brew connection version promoted from message
content`). If a Basestation reports it stays on v0, that is a client-side choice;
the server interoperates correctly at both v0 and v1.

The v1 additions this server understands are the SS-TPI talking-party
`mnemonic[34]` on `GROUP_TX` and `SETUP_REQUEST` (decoded per ETSI EN 300 392-9,
8-bit and 7-bit packed alphabets), and the `X-Brew-Mode` header
(`Terminal`/`Basestation`), tracked per client so terminals can be excluded from
registration pushes.

## SDS routing

Basestation sends SDS as two Brew packets with the same UUID:

```text
CALL_SHORT_TRANSFER(uuid, source ISSI, destination ISSI)
FRAME_SDS_TRANSFER(uuid, payload)
```

The server resolves the destination to the Basestation currently owning that ISSI, forwards both packets, then routes `FRAME_SDS_REPORT(uuid, status)` back to the originating Basestation. If the destination number is a currently affiliated GSSI instead, the SDS is multicast to the affiliated cells and reports are returned until the route expires.

SDS transaction state expires after 60 seconds.

## Group priority / pre-emption

By default a higher numeric priority wins (`higher_priority_number_wins = true`). If TG 91 currently has priority 3 and another cell starts TG 91 at priority 7, the server:

1. Generates `GROUP_IDLE` for the displaced call UUID using `preempt_cause`.
2. Sends it to the old transmitting cell and all routed listening cells.
3. Removes the old call/floor state.
4. Installs and forwards the new higher-priority call.

Equal/lower priority attempts are rejected while the floor is occupied. Set `higher_priority_number_wins = false` if your deployed Brew/TETRA profile uses inverse priority ordering.

## Private/simplex routing (experimental)

The following Brew call states are recognized and routed by call UUID:

- 4 SETUP_REQUEST
- 5 SETUP_ACCEPT
- 6 SETUP_REJECT
- 7 CALL_ALERT
- 8 CONNECT_REQUEST
- 9 CONNECT_CONFIRM
- 10 CALL_RELEASE
- 12 SIMPLEX_GRANTED
- 13 SIMPLEX_IDLE

`SETUP_REQUEST` establishes the route from the structured `BrewCircularCall` payload (source ISSI, destination ISSI, dialled number, priority, and — on v1 — the talking-party `mnemonic`); for payloads that cannot be fully structured it falls back to the first 8 bytes (`source_issi:u32 LE`, `destination_issi:u32 LE`). If the destination ISSI is registered on another Basestation, the call stays on the Brew side; thereafter control messages and traffic-channel frames may flow in either direction between the two participating cells until `CALL_RELEASE`.

If the destination ISSI is *not* a registered subscriber, the call is offered to the SIP subsystem (Brew -> SIP) instead of being rejected outright: `[[sip.routes]]` entries are matched against a dialled string, which is the `BrewCircularCall`'s ASCII `number` field when the caller set one, falling back to the destination ISSI rendered as decimal otherwise. This is how a mobile terminal dialling an outside-line-style number (e.g. "9" + a 10-digit PSTN number) reaches a SIP trunk: the terminal sends `destination = 0` with the dialled digits in `number` (this is how FlowStation encodes a PBX/phone call — see its `cc_bs/procedures/setup.rs`), a route like `match_pattern = "9*"` selects it, and an optional `strip_prefix = "9"` on the route removes the leading digit before it reaches an empty-`number` `sip_trunk` destination, so the trunk dials the bare 10 digits. See `[[sip.routes]]` in Configuration above.

**Duplex vs. PTT.** `build_circular_call_setup` (the server-originated `SETUP_REQUEST` for a SIP->Brew private call) sets `duplex=1` and `method=1` in the `BrewCircularCall` payload. There is no separate "PBX"/"phone" call type in TETRA CMCE to select instead (`communication` only has `P2p`/`P2Mp`/`P2MpAcked`/`Broadcast`, and `P2p` — already what this server sends — is correct for an individual call whether it's a radio-to-radio call or a bridged PSTN call); what actually matters is `duplex`/`method`. With both left at `0`, FlowStation's `cc_bs` presents the call as simplex with a PTT-style `TransmissionGrant` and non-hook signalling — the mobile terminal can only be "answered" by pressing PTT, never the real accept/green button, and audio doesn't behave like a normal duplex phone call even once picked up that way. `duplex=1` (full duplex) + `method=1` (hook signalling, i.e. the call requires an explicit user answer) make the terminal present and handle it as a genuine duplex phone call.

**Answering a Brew->SIP (MS-originated) call.** When a mobile terminal itself places the call and the SIP/PSTN side answers, the message that tells the MS "connected" is `CALL_CONNECT_REQUEST` (`build_circular_connect_request`, also with `duplex=1`/`method=1`) — *not* `CALL_CONNECT_CONFIRM`. FlowStation's `cc_bs` explicitly ignores `CALL_CONNECT_CONFIRM` for a call where the MS is the calling party (`fsm_on_network_circuit_connect_confirm` checks `calling_over_brew` and returns early otherwise); sending it left the terminal stuck showing "calling..." even after the far end had genuinely answered. `CALL_CONNECT_CONFIRM` remains correct for the opposite direction (SIP->Brew, in response to the ISSI's own `CALL_CONNECT_REQUEST`), where it's already what this server sends. The outbound `INVITE`'s `From`/`Contact` also now identify the call as `sip:<issi>@host` rather than a generic `sip:brew@host`, so the far end sees a real caller identity.

**Codec for a Brew->SIP call.** The transcoder for a Brew-originated leg is *not* started when the `INVITE` is sent — it's started once the SIP peer's `200 OK` actually arrives, using whichever of PCMU/PCMA that answer's own SDP picked (`BrewBridge::start_pending_media`, called from `on_sip_response`'s `200` case), not a guess made before the peer had even answered. Starting the transcoder early at a fixed assumption produced garbled audio in one direction and effectively nothing intelligible in the other whenever the peer answered PCMA instead of PCMU — the same bug the earlier "optimistic answer" note used to describe. The `RtpLeg` for this call, its Brew-side receive channel, and its target list are held in `BridgedLeg::pending_media` until the answer arrives; the leg's remote RTP address is also set explicitly from the answer's SDP at that point (`c=`/`m=audio`), rather than relying only on symmetric-RTP latching from the first inbound packet — the latter still applies as a fallback/NAT-safety net, but no longer as the *only* way this leg learns where to send audio.

Because current upstream Basestation does not yet expose a complete private-call Brew command path, this feature should be considered server-ready/experimental rather than end-to-end validated.

## Scope and security

This is a lab/experimental core, not a production TETRA SwMI. Digest authentication protects credentials from being sent directly but MD5 Digest is legacy authentication; enable the built-in `[tls]` support (or deploy behind a TLS-terminating proxy) or run on a trusted private network. The server currently has no persistent subscriber database, ACL policy, rate limiting, or HA state replication.

The dashboard is a separate listener with its own auth (`[dashboard.users]`, HTTP Basic) and TLS (`[dashboard.tls]`). Basic auth transmits credentials as reversible base64, so only enable `[dashboard.users]` together with `[dashboard.tls]` (or behind a trusted network) — never run dashboard auth over plain HTTP. Note the dashboard's Control panel can kick subscribers and restart/stop a Basestation BTS, so treat dashboard access as privileged. With no users configured the dashboard is open to anyone who can reach the port.

## Basestation connected/registered but no inter-BS calls

A subscriber `REGISTER` is not the same thing as a talk-group `AFFILIATE`. If the
server log contains `subscriber registered` but no `subscriber affiliated ... gssi=...`,
there is no affiliation table to route by. v0.2.1 therefore defaults
`fallback_broadcast_when_no_affiliations = true`: when a `GROUP_TX` arrives for a
GSSI with no recorded affiliations, it is sent to every other connected Basestation.
Once `AFFILIATE` messages are present, selective GSSI routing is used again.

If pressing PTT still produces no `routed GROUP_TX` line at the server, the problem is
upstream of the server: Basestation has not emitted the Brew `GROUP_TX`. Enable DEBUG
logging for Basestation's Brew entity/worker and look for `forwarding local call to
TetraPack` / `sent GROUP_TX`. SDS also requires Basestation's Brew SDS feature to be
enabled; otherwise Basestation intentionally ignores `SendSds`.

## Basestation Telemetry (experimental)

In addition to the Brew link, Basestation-based Basestations can optionally push a
one-way **Telemetry** stream (registrations, calls with carrier/timeslot, RF/DSP
quality, SDR/host health, SDS log, emergency alarms) over a second, BTS-initiated
WebSocket. This is a separate listener from Brew because the handshake shape
differs: single-step upgrade at `/` (no Digest challenge), optional HTTP **Basic**
auth, and a required `Sec-WebSocket-Protocol: bluestation-telemetry-v2` echo.

Enable it in `brew-server.toml`:

```toml
[telemetry]
enabled = true
listen = "0.0.0.0:9001"

[telemetry.users]
"100000001" = "change-me-telemetry1"

[telemetry.tls]
enabled = false
```

Leave `[telemetry.users]` empty to accept connections without auth. Point each
Basestation Basestation's `[telemetry]` config section at
`ws://<this server>:9001/` (or `wss://` with `telemetry.tls.enabled = true`).

Connected stations, their health, active calls (with carrier + timeslot), RF
quality, and telemetry-sourced SDS traffic appear on the dashboard below.
Active emergency alarms are surfaced as a banner at the top of the page.

Reverse-engineered from Basestation v0.4.0 source, not a published spec —
re-verify against whatever Basestation version you actually deploy.

## Basestation Control (experimental)

The **Control** channel is the bidirectional counterpart to Telemetry: the BTS
still initiates the WebSocket connection (subprotocol `bluestation-control-v1`),
but once connected an operator can push commands down it (kick a subscriber,
DGNA assign/deassign, inject/manage live SDS, clear an emergency, restart or
stop the Basestation service) and read back responses for the few command
types that define one (`SendSds`, `CommandA`, `KickMs`).

Enable it in `brew-server.toml`:

```toml
[control]
enabled = true
listen = "0.0.0.0:9002"

[control.users]
"100000001" = "change-me-control1"

[control.tls]
enabled = false
```

Point each Basestation Basestation's `[command]` config section at
`ws://<this server>:9002/`. Connected control-capable stations appear on the
dashboard with a command panel: Kick MS, DGNA assign/deassign, Clear
Emergency, live-SDS add/delete/clear, and Restart/Shutdown (both ask for
confirmation client-side, since they end the BTS process). `SendSds` /
`SendRawSdsType4` / `TestCmdB` take a raw hex-encoded payload — this server
does not encode SDS-TL PDUs for you.

Like Telemetry, this is reverse-engineered from Basestation v0.4.0 source;
re-verify against your deployed version.

## SIP / VoIP

This build adds a **SIP subsystem** alongside the Brew/TETRA core. It lets SIP
clients and SIP trunks connect, and bridges voice between SIP and the TETRA side
(brew mobile clients and basestation mobile stations). It is off by default;
enable it in `[sip]`:

```toml
[sip]
enabled = true
listen = "0.0.0.0:5060"          # UDP SIP signalling
advertised_host = ""             # public/reachable IP when behind NAT (empty = socket local addr)
rtp_port_min = 16000             # RTP relay media port pool
rtp_port_max = 17000
realm = "brew-server"
registration_ttl_seconds = 3600
max_call_duration_seconds = 14400 # force-end a SIP call (and its Brew leg, if bridged) past this; 0 disables
```

The subsystem provides:

- **SIP extensions** — user/pass accounts that REGISTER to this server. Digest
  (MD5) authentication is enforced on REGISTER and on INVITE. Provision them
  under `[sip.extensions.<user>]`:

  ```toml
  [sip.extensions.1001]
  password = "change-me-1001"
  display_name = "Reception"
  issi = 1001            # optional: map to a TETRA subscriber ISSI
  allow_outbound = true
  ```

- **SIP trunks** — peer VoIP gateways (Asterisk, an ITSP, another PBX). Three
  directions are supported: `outbound` (we REGISTER to the peer), `inbound` (the
  peer REGISTERs to us), and `peer` (static IP-authenticated, no registration).
  Outbound trunks answer the peer's 401/407 challenge automatically and
  re-register on the configured interval.

  ```toml
  [sip.trunks.asterisk]
  direction = "outbound"
  remote_host = "192.0.2.10:5060"
  username = "brew-trunk"
  password = "change-me-trunk"
  register_interval_seconds = 300
  enabled = true
  ```

- **Voice routes** — bridge calls between any two endpoints: SIP extension, SIP
  trunk, Brew private subscriber (ISSI), or Brew group (GSSI). Routes are
  evaluated top to bottom; the first enabled route whose `match_pattern` (and
  optional `from` restriction) matches the dialled destination wins.
  `match_pattern` is `*` (any), a trailing-`*` prefix, or an exact string.
  Matching always runs against the full dialled string; an optional
  `strip_prefix` then removes a leading literal before the string reaches an
  empty-`number` `sip_trunk` destination (an outside-line prefix like "9").
  This also covers a mobile terminal dialling a non-ISSI (PSTN) number: it
  arrives with `destination = 0` and the digits in the Brew `number` field,
  which is used as the dialled string for routing in that case.

  ```toml
  # Extensions, or a mobile terminal, dial 9 + number to break out via the
  # Asterisk trunk; strip_prefix drops the "9" so the trunk dials 10 digits.
  [[sip.routes]]
  name = "outbound-via-asterisk"
  match_pattern = "9*"
  strip_prefix = "9"
  to = { kind = "sip_trunk", trunk = "asterisk" }
  enabled = true

  # Calls in from the trunk are patched into TETRA group 1001.
  [[sip.routes]]
  name = "asterisk-to-tetra-group"
  match_pattern = "*"
  from = { kind = "sip_trunk", trunk = "asterisk" }
  to = { kind = "brew_group", gssi = 1001 }
  enabled = true

  # Dial 7 + ISSI from a SIP extension to reach a TETRA subscriber privately.
  [[sip.routes]]
  name = "ext-to-tetra-private"
  match_pattern = "7*"
  from = { kind = "sip_extension", user = "1001" }
  to = { kind = "brew_private", issi = 90 }
  enabled = true
  ```

Endpoint kinds for `to`/`from`: `{ kind = "sip_extension", user = "..." }`,
`{ kind = "sip_trunk", trunk = "...", number = "..." }` (number optional; the
dialled digits — after `strip_prefix`, if set — are used when omitted),
`{ kind = "brew_private", issi = N }`, `{ kind = "brew_group", gssi = N }`.
`strip_prefix` (default: none) is a route-level field, not part of the
endpoint, so it applies regardless of which `to` kind is used.

**Dashboard.** Two pages, linked from the main dashboard:

- `/sip` — live panel: extension registrations, trunk status (up / registering /
  failed / down) with active call counts, and active calls. JSON at `/api/sip`.
- `/sip-config` — read-only view of the provisioned extensions, trunks and
  routes (passwords are never shown). JSON at `/api/sip/config`. Edit the
  `[sip]` section of the config file to change provisioning; the server watches
  the file and restarts to apply.

**Media / codecs.** SIP legs are negotiated to G.711 (PCMU/PCMA) and relayed by
a built-in symmetric-RTP forwarder that latches each peer's real source address
(NAT-safe). SIP↔SIP trunking works end to end. For **SIP↔TETRA audio**, TETRA
carries ACELP voice inside Brew traffic frames; this server includes an
ACELP↔G.711 transcoder (vendoring the ETSI EN 300 395-2 reference codec, see
`third_party/tetra-codec/`) so a SIP↔TETRA call carries real audio in both
directions, not just signalling.

## Federation (server-to-server)

Multiple brew-server instances can be linked together so calls, SDS and
subscriber/group registrations reach a remote site's Basestations and mobile
stations -- e.g. a chain (A-B-C) or a star (a hub with several spokes). A peer
link connects and authenticates exactly like a Basestation does, over the same
Brew WebSocket protocol, just tagged `X-Brew-Mode: Peer`. Enable it in
`[federation]`:

```toml
[federation]
enabled = true

[[federation.peers]]
name = "site-b"
remote_host = "10.0.0.20:9000"   # the peer's Brew listener, same port a Basestation uses
path = "/brew"
username = "9000001"             # only needed if the peer has [auth] enabled
password = "change-me-federation"
reconnect_interval_seconds = 15
enabled = true
```

Each `[[federation.peers]]` entry is one **outbound** link this server dials
(with reconnect on failure/drop). The far end needs no matching peer entry to
*accept* a connection -- an inbound link just authenticates like a Basestation
would (HTTP Digest if `[auth]` is enabled there) and is recognized as a peer
from the `X-Brew-Mode: Peer` header, same as any other Brew connection.

**How routing works.** There is no separate federation routing table to
configure (which ISSI/GSSI lives behind which peer): registrations propagate
peer to peer automatically. When a subscriber registers or affiliates to a
group anywhere in the topology, every server relays what it learns to its
*other* peers (never back out the link it arrived on), so the whole tree
converges on a shared picture of who is reachable where -- similar in spirit
to distance-vector routing. A private/group call or SDS to a destination not
registered locally then routes to whichever peer link that destination was
learned through, the same way it already routes to any other connected
client; there is no federation-specific call/SDS handling at all, hop to hop
it just resolves the destination and forwards. A newly (re)connected peer is
sent a full snapshot of everything this server currently knows so it isn't
blind to registrations that predate the link.

**Topology.** This propagation is correct for any loop-free topology -- a
chain or a star, i.e. any tree of peer links. A topology with a cycle (e.g. a
full mesh, or two independent paths between the same two servers) is **not**
safe with the split-horizon relaying implemented here: it can loop
indefinitely. Stick to a tree.

**Scope.** This covers private/group call routing and SDS forwarding across
peers. Basestation telemetry (RF/DSP health, per-station registration lists)
is not relayed across federation links in this version -- each server's
dashboard only shows telemetry for Basestations connected directly to it.

## Basestation locations

`[bts_locations]` gives each Basestation a fixed marker on the `/map`
dashboard page, distinct from the mobile-station position markers that come
from decoded LIP beacons:

```toml
[bts_locations."1000001"]
name = "Athens HQ"
lat = 37.9917
lon = 23.7640
```

The table key is the numeric Brew username that Basestation authenticates
with under `[auth.users]` (same 1-7 digit rule) -- whichever live connection
logs in as that identity is matched automatically, no separate station ID
needed. `/api/bts-locations` merges the fixed `name`/`lat`/`lon` with live
connection state (IP address, connected/offline), and the map popup shows
all of it. Manage entries from the `/settings` page's "Basestation
Locations" panel, or directly in the raw TOML.

## APRS

Decoded mobile-station LIP positions (the same fixes plotted on the
dashboard's `/map`) can also be forwarded to APRS-IS. Configure `[aprs]`:

```toml
[aprs]
enabled = true
server = "rotate.aprs2.net:14580"
callsign = "MYCALL-10"
passcode = "12345"
symbol_table = "/"
symbol_code = "j"
comment = "TETRA MS via brew-server"
object_name_prefix = "MS"
min_report_interval_seconds = 60
reconnect_interval_seconds = 15
```

`callsign`/`passcode` are this server's *own* APRS-IS login -- not a
per-mobile-station credential. Every reporting ISSI is sent as an APRS object
(`;MS90      *...`, named from `object_name_prefix` + the ISSI, padded/
truncated to APRS's fixed 9-character object name) under that one login, the
same approach real DMR/D-STAR-to-APRS gateways use. `passcode` is not derived
here; obtain it the same way any APRS client does, tied to `callsign`.
`min_report_interval_seconds` rate-limits how often any single ISSI's object
is re-sent, so a noisy beacon source cannot flood APRS-IS.

Can be toggled/edited live from the dashboard's `/settings` raw-TOML editor,
like any other setting.

## Web monitoring dashboard

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
  below, which is a historical event log.
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
"Basestation Telemetry" above, which is where the carrier-timeslot data comes
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
MS, DGNA, live SDS, clear emergency, restart/shutdown) — see "Basestation
Control" above. Live per-station state (health, active calls, RF quality,
registrations, the registration event log) is in-memory and resets when the
BTS's telemetry/control connection restarts; calls, SDS, and the Telemetry SDS
Log survive a server restart when `[storage]` is enabled (see "Persistent
history" above).
