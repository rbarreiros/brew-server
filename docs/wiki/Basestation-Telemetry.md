# Basestation Telemetry (experimental)

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
quality, and telemetry-sourced SDS traffic appear on the [dashboard](Dashboard.md).
Active emergency alarms are surfaced as a banner at the top of the page.

Reverse-engineered from Basestation v0.4.0 source, not a published spec —
re-verify against whatever Basestation version you actually deploy.
