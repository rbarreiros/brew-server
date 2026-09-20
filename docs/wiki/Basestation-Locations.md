# Basestation locations

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
