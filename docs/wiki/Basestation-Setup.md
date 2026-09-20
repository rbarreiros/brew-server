# Basestation side

Configure each Basestation's Brew transport to point at the server host/port, use endpoint `/brew` (or `/brew/`), subprotocol `brew`, and set the matching Digest username/password. With Digest credentials configured, current Basestation performs:

1. `GET /brew/` without credentials.
2. Server returns `401` with a Digest challenge.
3. Basestation retries with `Authorization: Digest ...`.
4. Server returns a one-time path such as `/brew/session/<token>`.
5. Basestation upgrades that path to WebSocket with subprotocol `brew`.
