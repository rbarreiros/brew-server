# Protocol version negotiation

The server implements **Brew protocol version 1**. Clients may advertise the
version they speak with an `X-Brew-Version` header on the discovery `GET`:

- A matching (or lower, still-supported) version is accepted; the server echoes
  `X-Brew-Version` on the `200` response.
- An unsupported version gets `426 Upgrade Required`.
- A **missing** header is accepted for backward compatibility, and the version is
  then determined per connection from the message stream.

Because the WebSocket handshake itself carries no version header, the version is
a **per-connection** property that starts at v0 and is *promoted lazily* to v1
the first time a v1-shaped call-control message (one carrying the `mnemonic[34]`
tail) is observed. This mirrors how Basestation resolves the version and is
logged once per connection (`Brew connection version promoted from message
content`). If a Basestation reports it stays on v0, that is a client-side choice;
the server interoperates correctly at both v0 and v1.

The v1 additions this server understands are the SS-TPI talking-party
`mnemonic[34]` on `GROUP_TX` and `SETUP_REQUEST` (decoded per ETSI EN 300 392-9,
8-bit and 7-bit packed alphabets), and the `X-Brew-Mode` header
(`Terminal`/`Basestation`), tracked per client so terminals can be excluded from
registration pushes.
