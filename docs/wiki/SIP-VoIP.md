# SIP / VoIP

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
