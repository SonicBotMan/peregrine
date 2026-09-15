# Changelog

## Unreleased (post-alpha.3 hardening rounds)

Five engineering rounds driven by the QA-E2E audit and backlog
triage (B34/B38/B13/B24, B59, B52/B56/B50, B39/B41/B42,
B36-residual).

### Fixed

- **416 self-heal**: a resume whose offset doesn't settle as
  "already complete" (sparse-preallocated sink, mutated file,
  lying mirrors) restarts once from zero instead of failing
  forever. `replayed_from_zero` accounting keeps progress honest
  when a server ignores Range and rewrites the whole body (B34).
- **HLS finalize on cancel/pause**: an interrupted merge now
  salvages whatever segments landed instead of leaking them;
  permanent 4xx on a segment fails fast instead of retrying
  forever; EXT-X-MAP init segments get bounded retries (B50/B52
  sibling fixes in engine-ftp: resume-missing and SIZE-skew
  errors are human-readable, split by direction).
- **BT cross-restart purge**: a torrent removed with files kept
  can be re-added cleanly after a daemon restart (precise data
  paths tracked in a purge side table; magnet `bt://` links are
  rejected up front with a clear message) (B59).
- **Single-stream resume validators**: `If-Range` revalidation on
  every resume; the downgrade path keys its validator row on the
  caller's URL (was: unreachable under the redirect target);
  `Last-Modified` validators must be valid RFC 7231 dates
  (garbage degrades to validator-less resume) (B36 + residual).
- **Daemon shutdown**: second Ctrl-C no longer kills the process
  mid-cleanup; stale-socket probe heals at boot; orphan task rows
  GC'd when their sink file is gone (B38/B13/B24).

### Changed

- Desktop: UI action failures raise a transient error banner
  (B39); the sidecar daemon logs to `<log dir>/peregrined.log`
  (release installs have no console) (B41); the deb CI gate
  verifies `Depends` actually includes webkit2gtk (B42).

## v2.0.0-alpha.3 — 2026-09-15

Packaging repair release over alpha.2 (the alpha.2 tarball shipped
the thin client alone — it cannot run without its daemon).

### Fixed

- **The headless tarball now ships the FULL binary set** — `pg`,
  `peregrined`, `peregrine-mcp` (alpha.2's tarball had only `pg`).
  The `.deb` assets list matches (verified by unpacking the built
  deb). `depends` is now `$auto` so dpkg keeps the glibc floor
  check. sha256 files carry the bare tarball name so
  `sha256sum -c` works at any download location, and the tarball
  is built reproducibly (stable ownership/ordering, mtime pinned
  to the commit timestamp, `gzip -n`).
- Test suite: a full-chain regression test pins the re-add
  instant-complete contract (real daemon + real store + real
  engine + real HTTP server): re-adding a completed (url, sink)
  completes with ZERO server fetches and absolute progress.

## v2.0.0-alpha.2 — 2026-09-15

Fix-and-polish round over alpha.1, all from real-machine
acceptance (GUI walkthrough + CLI speed benchmarks) findings.

### Fixed

- **Re-add of an already-downloaded target no longer reports
  `received=0`** — the scheduler sink now lifts its seed to the
  engine's disk truth on session start (segmented skip-all
  completion included), reports ABSOLUTE progress (base + session
  delta) instead of the session-only delta, and keeps the resume
  base monotone across same-sink downgrades. Verified by
  revert-red regression tests (`readd_*` in scheduler suite).
- **Completed tasks keep their segment plan rows** —
  `GET /tasks/{id}/segments` now returns the terminal segment
  table instead of an empty list (the GUI segment panel used to
  blank out on completion). Re-adding a completed (url, sink)
  resumes onto the done rows and completes with ZERO network
  fetches. Rows are purged on task removal as before.
- **`pg list` no longer truncates task ids** — the 21-char id is
  the operating handle for `pg get/pause/remove`; the URL column
  now absorbs the terminal width instead (`COLUMNS`, 80 fallback,
  head-first clip keeping the filename tail).

### Added

- CLI: shell completions (`pg completions`), man pages
  (`pg gen-man`), TCP client transport (`--socket tcp:HOST:PORT` /
  `PGRG_SOCKET`).
- systemd units (`assets/peregrined.service`, `assets/peregrined@.service`)
  for daemon deployment (per-user and templated multi-instance).
- Desktop: single-instance guard, sidecar daemon auto-start with
  explicit-path fallback; shell-completion generators.
- README: GUI section with real screenshots (live segment panel,
  completed list) + real-machine benchmark chart (tele2 mirror,
  3.1× single-stream, md5-verified).

### Review process

Each fix round: R1 → independent R2 (delegate) → R3 reflection,
dispositions in `docs/reviews/` (`P1-readd-received0-*.md`,
`P2-contract-fixes-*.md`). 268 workspace tests, clippy
`-D warnings` clean.

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
