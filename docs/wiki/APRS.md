# APRS

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
