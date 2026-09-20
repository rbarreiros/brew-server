# brew-server

Experimental Rust Brew core for linking two or more MidnightBlue Basestation or Flowstation TETRA base stations.

Reference spec from https://wiki.tetrapack.online/tetra/specifications/brew/

- **What's new:** see [CHANGELOG.md](CHANGELOG.md)
- **Configuration and feature docs:** see the [wiki](https://github.com/ysamouhos/brew-server/wiki)

## Run directly

Requires a Rust toolchain and a C compiler (the vendored ACELP codec in
`third_party/tetra-codec/` is compiled by `build.rs`).

```bash
cargo run --release -- brew-server.toml
```

## Run via Docker

```bash
docker compose up --build
```

## Health check

```bash
curl http://127.0.0.1:9000/healthz
```
