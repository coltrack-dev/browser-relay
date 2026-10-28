# BrowserRelay

Self-hosted remote Chromium browser, primarily implemented in Rust.

## Status

Phase 2 video slice: Phase 1 browser runtime/API plus optional GStreamer `webrtcbin` video transport for one viewer. Audio and input control are not implemented.

## Layout

- `crates/relay-server` — Axum HTTP backend (`127.0.0.1:8099` по умолчанию, через `BROWSER_RELAY_PORT`)
- `crates/browser-runtime` — Chromium process ownership
- `crates/browser-control` — safe navigation URL validation
- `crates/session-manager` — typed session state transitions
- `crates/relay-protocol` — future API data types
- `crates/media-streaming` — single-viewer H.264/WebRTC signaling and optional GStreamer backend
- `web/` — frontend placeholder
- `docs/` — technical documentation

Chromium proxy задаётся через `BROWSER_RELAY_PROXY` в формате `socks5://host:port`. Локальные ресурсы и Docker fixture обходят proxy; CDP pipe и WebRTC signaling остаются локальными.

See [AGENTS.md](AGENTS.md) for implementation rules.
