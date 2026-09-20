# Basestation connected/registered but no inter-BS calls

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
