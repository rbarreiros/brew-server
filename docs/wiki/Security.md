# Scope and security

This is a lab/experimental core, not a production TETRA SwMI. Digest authentication protects credentials from being sent directly but MD5 Digest is legacy authentication; enable the built-in `[tls]` support (or deploy behind a TLS-terminating proxy) or run on a trusted private network. The server currently has no persistent subscriber database, ACL policy, rate limiting, or HA state replication.

The dashboard is a separate listener with its own auth (`[dashboard.users]`, HTTP Basic) and TLS (`[dashboard.tls]`). Basic auth transmits credentials as reversible base64, so only enable `[dashboard.users]` together with `[dashboard.tls]` (or behind a trusted network) — never run dashboard auth over plain HTTP. Note the dashboard's Control panel can kick subscribers and restart/stop a Basestation BTS, so treat dashboard access as privileged. With no users configured the dashboard is open to anyone who can reach the port.
