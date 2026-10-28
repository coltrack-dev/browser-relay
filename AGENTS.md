# BrowserRelay — Instructions for Codex

## Project scope
BrowserRelay is a self-hosted remote browser. Chromium runs on a headless Linux server, while a web client connects to a remote interactive session. The planned stack is Rust (Tokio, Axum, CDP), Chromium on a virtual display, Vue 3/TypeScript and, in a later phase, GStreamer/WebRTC with H.264 video and Opus audio.

The project is **not branded for any specific website or video platform**. Do not use third-party video-service trademarks or their derivatives in project names, repository paths, package names, UI branding, or container names.

## Current milestone: Phase 1 only
Implement a reliable local browser runtime, not streaming:
- Rust Cargo workspace with modular crates;
- non-root Chromium lifecycle on Linux/Xvfb;
- Chrome DevTools Protocol (prefer `chromiumoxide` after checking compatibility);
- single-session state machine and lifecycle;
- Axum HTTP endpoints for start, stop, status, navigation and reload;
- minimal Vue 3 frontend for URL entry and session controls;
- Docker/Compose configuration and Russian-language documentation.

Do **not** claim video streaming, audio delivery, mouse streaming, or WebRTC works in Phase 1. Reserve interfaces for future media integration only when genuinely useful.

## Architecture
- `crates/relay-server`: HTTP API, composition and lifecycle wiring.
- `crates/browser-runtime`: Chromium/Xvfb process ownership and cleanup.
- `crates/browser-control`: CDP communication and browser actions.
- `crates/session-manager`: typed session states and safe transitions.
- `crates/media-streaming`: future media transport; no fake implementation.
- `crates/relay-protocol`: shared wire types and DTOs.
- `web/`: Vue 3/TypeScript frontend.
- `docs/`: documentation in Russian.

Keep a single deployable backend (not microservices). Use descriptive names, explicit typed errors, and limited dependencies. Never add Redis, Kafka, PostgreSQL or Kubernetes without a concrete requirement.

## Security invariants
- Never request sudo, root privileges, privileged containers or broad host capabilities just to make development work.
- Do not disable AppArmor or change user namespace sysctls.
- Do not use `--dangerously-bypass-approvals-and-sandbox` or weaken the Codex sandbox.
- Chromium must run as an unprivileged user; do not use `--no-sandbox` as the default solution.
- CDP must not be exposed on a public interface; prefer pipe or loopback.
- Do not interpret user-provided URLs as commands, execute arbitrary client JavaScript, or expose an unauthenticated browser-control service to the public network.
- No secrets, credentials, cookies, or user-page content in logs or source control.

## Tooling and blocked environment
The user's Codex CLI sandbox currently fails during initialization with:
`bwrap: loopback: Failed RTM_NEWADDR: Operation not permitted`.
Shell execution and file edits through Codex can be unavailable, even though ordinary user-terminal commands work.

- Attempt a trivial non-destructive operation at most once per session to identify the capability.
- If tool execution or workspace edits fail: stop tool retries; report exactly what failed. Do not claim files were written, built, or tested.
- Do **not** generate huge source-code dumps in chat unless the user explicitly requests text/diffs.
- Instead, provide the next actionable plan or a concise, manually applicable patch only if asked.
- Never seek higher privileges or alter OS security to work around this limitation.

## Development process
- Work incrementally, starting with a compilable Phase 1 slice.
- Prefer modifying real files over describing modifications, whenever tools work.
- Check existing files before edits and preserve user changes.
- Prefer `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `docker compose config` when execution is available.
- Label checks not run as **NOT RUN**, never **PASS**.
- Do not automatically proceed to Phases 2–4.
- Do not commit or push unless explicitly requested.

## Response style
Respond to the user in Russian. Keep technical identifiers and code in English. Report the files actually changed, tests actually executed, known limitations, and immediate next step.
