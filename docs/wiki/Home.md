# brew-server wiki

Configuration and feature documentation, one page per section/function. For
what's new in each release see [CHANGELOG.md](../../CHANGELOG.md); for how to
build/run the server see the top-level [README.md](../../README.md).

## Getting started

- [Compatibility note](Compatibility-Note.md) — current limitations vs. the Brew spec
- [Configuration](Configuration.md) — core `brew-server.toml` settings
- [TLS](TLS.md) — native HTTPS/WSS termination
- [Basestation side](Basestation-Setup.md) — pointing a Basestation at this server
- [Protocol version negotiation](Protocol-Version-Negotiation.md) — Brew v0/v1

## Call and message routing

- [SDS routing](SDS-Routing.md)
- [Group priority / pre-emption](Group-Priority.md)
- [Private/simplex routing](Private-Simplex-Routing.md) (experimental)
- [Troubleshooting](Troubleshooting.md) — registered but no inter-BS calls

## Optional subsystems

- [Basestation Telemetry](Basestation-Telemetry.md) (experimental)
- [Basestation Control](Basestation-Control.md) (experimental)
- [SIP / VoIP](SIP-VoIP.md)
- [Federation](Federation.md) — server-to-server linking
- [Basestation locations](Basestation-Locations.md) — map markers
- [APRS](APRS.md) — forward MS positions to APRS-IS
- [Persistent history](Persistent-History.md) — the `[storage]` log and `brew-history` tool

## Operations

- [Web monitoring dashboard](Dashboard.md)
- [Security](Security.md) — scope, auth, what this server is not
