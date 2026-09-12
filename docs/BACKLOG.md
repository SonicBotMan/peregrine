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
