# Changelog

## v2.0.0-alpha.1 — 2026-09-13

Complete rewrite. Peregrine v1 (Node/TypeScript) is frozen at
`v0.1.0-m0`; v2 is a Rust workspace with a daemon-first
architecture.

### Engines

- **HTTP/HTTPS** — dynamic segmented download (auto splits by size,
  re-splits on segment completion), ETag/Last-Modified validation,
  kill -9 crash resume (SQLite WAL segment table), per-task +
  daemon-wide rate limiting (token bucket, live-adjustable).
- **HLS** — VOD playlists (EXT-X-KEY AES-128 decryption, byterange
  support) and **live recording** (sliding-window follow until
  ENDLIST, offline-tolerant).
- **FTP** — suppaftp, REST resume, EPSV/PASV passive mode.
- **BitTorrent** — librqbit embedded (single-process, no rqbit
  service): magnet + .torrent (file:// and http), multi-file
  torrents into one output folder, refcounted purge (same torrent
  in two tasks never destroys the sibling's data), offline-safe
  tests (DHT off in `BtEngine::offline()`; production default has
  DHT on but never persisted without explicit opt-in).

### Daemon & clients

- **Daemon** — axum REST over UDS socket and/or TCP
  (`pg daemon [--tcp] [--socket]`), SQLite task rows, tokio
  broadcast event bus, WS `/events` stream for the GUI.
- **Scheduler** — priority queue (high/normal/low), max-concurrent
  slots, per-engine routing, live limit pokes.
- **CLI** — `pg add/list/status/pause/resume/remove --purge/limit/
  set-limit` against the daemon socket; same-source path
  resolution (PGRG_SOCKET → XDG_RUNTIME_DIR → /tmp).
- **GUI** — Tauri desktop app: task list with live progress,
  add/pause/resume/remove, global throttle. (Separate
  `ui/` workspace; not shipped in the headless artifacts.)
- **MCP server** — agent-native control: 10 tools, 3 resource
  templates (`tasks://`, `task://{id}`, `settings://`), push
  notifications over the daemon WS bridge (backpressure-tolerant:
  slow consumers drop progress pushes instead of killing the
  bridge). stdio and streamable-HTTP transports.

### Packaging

- `scripts/package.sh` — stripped tarball (+ .deb via cargo-deb)
  from one command.
- CI: `release.yml` builds Linux artifacts on `v*` tags;
  `desktop.yml` guards the GUI workspace.

### Known limits (alpha)

- BT per-task rate limiting is engine-wide only (librqbit
  ratelimits hook is a BACKLOG item).
- MCP `subscriptions/listen` (newer spec) not yet supported —
  legacy `resources/subscribe` only.
- No Windows/macOS artifacts yet (Linux x86_64 only).
- FTPS not implemented (plain FTP + passive mode).

### Review process

Every milestone went through R1 (self) → R2 (independent delegate
review) → R2' (post-fix re-review) → R3 (reflection) with written
dispositions in `docs/reviews/`. 258 workspace tests, clippy
`-D warnings` clean.
