# Federation (server-to-server)

Multiple brew-server instances can be linked together so calls, SDS and
subscriber/group registrations reach a remote site's Basestations and mobile
stations -- e.g. a chain (A-B-C) or a star (a hub with several spokes). A peer
link connects and authenticates exactly like a Basestation does, over the same
Brew WebSocket protocol, just tagged `X-Brew-Mode: Peer`. Enable it in
`[federation]`:

```toml
[federation]
enabled = true

[[federation.peers]]
name = "site-b"
remote_host = "10.0.0.20:9000"   # the peer's Brew listener, same port a Basestation uses
path = "/brew"
username = "9000001"             # only needed if the peer has [auth] enabled
password = "change-me-federation"
reconnect_interval_seconds = 15
enabled = true
```

Each `[[federation.peers]]` entry is one **outbound** link this server dials
(with reconnect on failure/drop). The far end needs no matching peer entry to
*accept* a connection -- an inbound link just authenticates like a Basestation
would (HTTP Digest if `[auth]` is enabled there) and is recognized as a peer
from the `X-Brew-Mode: Peer` header, same as any other Brew connection.

**How routing works.** There is no separate federation routing table to
configure (which ISSI/GSSI lives behind which peer): registrations propagate
peer to peer automatically. When a subscriber registers or affiliates to a
group anywhere in the topology, every server relays what it learns to its
*other* peers (never back out the link it arrived on), so the whole tree
converges on a shared picture of who is reachable where -- similar in spirit
to distance-vector routing. A private/group call or SDS to a destination not
registered locally then routes to whichever peer link that destination was
learned through, the same way it already routes to any other connected
client; there is no federation-specific call/SDS handling at all, hop to hop
it just resolves the destination and forwards. A newly (re)connected peer is
sent a full snapshot of everything this server currently knows so it isn't
blind to registrations that predate the link.

**Topology.** This propagation is correct for any loop-free topology -- a
chain or a star, i.e. any tree of peer links. A topology with a cycle (e.g. a
full mesh, or two independent paths between the same two servers) is **not**
safe with the split-horizon relaying implemented here: it can loop
indefinitely. Stick to a tree.

**Scope.** This covers private/group call routing and SDS forwarding across
peers. Basestation telemetry (RF/DSP health, per-station registration lists)
is not relayed across federation links in this version -- each server's
dashboard only shows telemetry for Basestations connected directly to it.
