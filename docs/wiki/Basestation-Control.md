# Basestation Control (experimental)

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
