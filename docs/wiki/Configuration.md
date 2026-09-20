# Configuration

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
from the Brew API above — see [Web monitoring dashboard](Dashboard.md) for auth and TLS
details. Use a different username/password for each Basestation. The Brew username is an HTTP Digest identity that must be **numeric and at most 7 digits** (a connection presenting a longer or non-numeric username is refused); it does not have to equal a radio ISSI, though a numeric site identity is convenient.
