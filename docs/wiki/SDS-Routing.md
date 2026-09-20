# SDS routing

Basestation sends SDS as two Brew packets with the same UUID:

```text
CALL_SHORT_TRANSFER(uuid, source ISSI, destination ISSI)
FRAME_SDS_TRANSFER(uuid, payload)
```

The server resolves the destination to the Basestation currently owning that ISSI, forwards both packets, then routes `FRAME_SDS_REPORT(uuid, status)` back to the originating Basestation. If the destination number is a currently affiliated GSSI instead, the SDS is multicast to the affiliated cells and reports are returned until the route expires.

SDS transaction state expires after 60 seconds.
