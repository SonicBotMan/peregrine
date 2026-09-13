# Backlog & Deferred Decisions

Findings reviewed and consciously deferred, with reasons. Revisit at the
milestone noted. (R1 review, 2026-02-27)

| # | Item | Deferred to | Reason |
| --- | ------ | ------------- | -------- |
| B1 | `EngineEvent::TaskProgress` coalescing (high-frequency events flood the 1024-slot bus) | M2 | No producers yet; PROPOSAL §5 already mandates coalescing when events are wired |
| B2 | Config file format decision (KDL vs RON vs TOML) | M5 | No config surface in M0; deciding now would be speculation |
| B3 | `rust-version` pin in Cargo.toml (vs rust-toolchain.toml) | M6 (CI round) | rust-toolchain.toml already locks 1.96.1 locally; CI picks it up |
| B4 | Long aliases for version flags (`-V`/`--version` on subcommands) | M6 | Zero-value polish until the command surface grows |
| B5 | `pg --json` flag for machine-readable output | M2 | MCP bridge is the real machine interface; premature now |
| B6 | Event bus capacity benchmarking (1024 slots) | M2 | Benchmarks need real event production rates |
| B7 | ~~`ProbeInfo` lacks real-Range validation~~ **DONE (M1-a, R1+R2 reviewed)**: probe now issues `Range: bytes=0-0` and decides `accept_ranges` by observed 206/200, with `Content-Range` total preferred over HEAD `Content-Length`; covered by probe.rs `probe_downgrades_servers_that_ignore_range` / `probe_upgrades_servers_that_honor_range_without_advertising` / `probe_trusts_content_range_over_head_length` | — | Completed in M1-a engine-http round |
| B8 | https→http cross-scheme redirect downgrade policy | M1 (auth headers) | Harmless without credentials; re-examine when probe/download grows auth headers (R2 self-review) |
| B9 | `ApiError::DuplicateEngine` on the IPC surface (it is a wiring bug, not a client-visible condition) | M2 (event stream) | Harmless today; revisit when events start carrying ApiError to clients (R2 self-review) |
| B10 | `serve` error path leaves the socket file; graceful shutdown has no overall deadline (a slow in-flight request can hang shutdown forever) — wrap `axum::serve` in `tokio::time::timeout(10s, ..)` and clean up on error too | M1 | Stale takeover self-heals on next start; deadline matters once real traffic exists (M2 reviewer P2-6) |
| B11 | libc double-version in graph: workspace pins `libc = "1.0.0-alpha.4"` (alpha pre-release) while tokio transitively uses `libc 0.2.189` — stopgap for a broken local mirror index | M1 | GitHub CI has Cargo.lock so builds reproduce; revert to 0.2 line when the local mirror recovers (M2 reviewer P2-8) |
| B12 | `ApiError::Io` keeps only a string; `io::ErrorKind` is lost, callers cannot branch on NotFound/PermissionDenied | M2 (with B9) | Structural change belongs with the IPC error surface, not before it (M2 reviewer P2-9) |
| B13 | Socket path hardening batch: XDG_RUNTIME_DIR owner-uid/0700 validation; /tmp fallback symlink rejection + uid ownership; residual uds.rs TOCTOU (stat→unlink handover windows, could rename-to-private-then-unlink); bind→chmod 0700 window for explicit `--socket` paths (umask tightening) | M1 hardening or M2 | User-mode today; all are defense-in-depth against anomalous environments (consolidates M2 reviewer P2-1/2/3 + P2-11) |
| B14 | Supply-chain gate: add cargo-deny (or cargo-audit) to CI — advisories + licenses + the B11 libc anomaly would surface automatically | M1 CI round (follow-up PR) | Explicitly recommended by M0 R2 review once dependencies started arriving |
| B15 | Content-Disposition parser: semicolons inside quoted filenames (currently dropped, not mis-parsed — test fixes the behavior) and `filename*` RFC 5987 encoding | M1-b/M2 | No consumer of the filename yet; parser hardening lands with real-world URL testing |
| B16 | HEAD-refusing servers (405/501): promote the ranged GET to primary probe (aria2 route) instead of failing | M1-c | Download path landed in M1-b (`download.rs`); fold the ranged-GET-primary fallback into the M1-c planner work where it will actually be exercised |
| B17 | Enable HTTP/2 ALPN in hyper-rustls for CDN-hosted large files | M2+ | HTTP/1.1 negotiates everywhere today; measure before enabling |
| B18 | Test: `uds_client` 5s timeout covering the response body (mock daemon that stalls mid-body) | M1-c | cli has no tests dir yet; add together with the first real RPC the client carries (download RPC lands with M1-c's task wiring) |
| B19 | M1-b leftovers (R1 self-review): `write_mode` is a string comparison in `download.rs` (should be an enum); `open_sink` append mode without `create(true)` yields a bare ENOENT instead of a helpful "resume target missing" error | M1-c refactor pass | Both are local readability issues, not correctness |
| B20 | `parse_content_range` ignores the END value of `bytes S-E/T` (only S and T are consumed); E could be validated against the resume offset as a second corruption guard (M1-b R2 P3-6) | M1-c planner | The planner issues bounded ranges and will consume E natively; adding validation now would duplicate that |
| B21 | Download redirect handling: 3xx with a Location lacking `..` normalization / relative resolution edge cases, and non-GET 304/305 semantics, are not specially covered beyond probe's shared logic | M1-c | Probe and download share the chase loop; only divergence-worthy when M1-c adds per-segment redirect budgets (M1-b R2 P3-7) |
