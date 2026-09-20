# TLS

Brew can terminate TLS natively so Basestations connect over `wss://` / `https://` without a reverse proxy. Enable it in `[tls]`:

```toml
[tls]
enabled = true
cert_path = "/etc/brew-server/tls/cert.pem"
key_path = "/etc/brew-server/tls/key.pem"
```

`cert_path` is a PEM certificate chain (leaf first, then any intermediates) and `key_path` is the matching PEM private key (PKCS#8 or RSA). When `enabled = true` the listener serves HTTPS on the same `listen` address, so clients, the discovery GET, session WebSockets, and the dashboard all move to `https://`/`wss://`.

Generate a self-signed cert for lab use:

```bash
mkdir -p tls
openssl req -x509 -newkey rsa:2048 -nodes -days 365 \
  -keyout tls/key.pem -out tls/cert.pem -subj "/CN=brew-server"
```

For Docker, mount the certs (the provided `docker-compose.yml` mounts `./tls` to `/etc/brew-server/tls`) and set the paths accordingly.
