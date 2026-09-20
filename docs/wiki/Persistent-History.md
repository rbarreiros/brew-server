# Persistent history

Completed calls and SDS are written to an append-only binary log (`bincode`-framed,
crash-safe on read) and replayed on startup, so call/SDS history and counters
survive restarts. Configured under `[storage]`:

```toml
[storage]
enabled = true
path = "brew-history.bin"
```

It keeps everything with no rotation. A torn trailing record from a hard crash
is detected and skipped.

SDS entries observed on a Basestation [Telemetry](Basestation-Telemetry.md)
channel (`SdsLog`) are also appended to the same log, tagged with the
reporting station, so the Telemetry SDS Log survives a server restart instead
of resetting with the BTS's live in-memory state.

Read the log with the bundled `brew-history` tool:

```bash
brew-history brew-history.bin            # readable text
brew-history brew-history.bin --json     # pipe into jq
```
