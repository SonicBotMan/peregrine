# Contributing to Peregrine

🦅 Thanks for considering a contribution. Peregrine is a young project
with an unusually high verification bar — this document tells you
exactly what "done" means here.

## Development setup

```bash
# Toolchain is pinned — CI and rust-toolchain.toml must agree (1.96.1).
rustup toolchain install 1.96.1

# System deps (Linux desktop shell)
sudo apt install libwebkit2gtk-4.1-dev libappindicator3-dev librsvg2-dev patchelf

# Frontend deps
cd apps/desktop && pnpm install

# Local run: vite (5199) + sidecar (8420) + desktop shell
pnpm tauri dev
```

A daemon sidecar binary is staged into
`apps/desktop/src-tauri/binaries/` by CI; when the server crate
changes locally, refresh it manually:

```bash
cargo build --locked -p peregrine-server
cp target/debug/peregrined \
  apps/desktop/src-tauri/binaries/peregrined-$(rustc -vV | sed -n 's/host: //p')
```

## The gates (all must be green)

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --locked --workspace
cd apps/desktop
pnpm exec svelte-check --tsconfig ./tsconfig.app.json   # 0 errors
pnpm exec vitest run                                    # 28+ pass
```

CI runs exactly these on every push (`ci.yml`, `desktop.yml`).
`rust-toolchain.toml` pins the toolchain — CI refuses drift.

## The review bar: three rounds

Every substantive change goes through 自查 → 独立复审 → 反思
(self-review → independent review → reflection), recorded in
`docs/reviews/`. For community PRs the maintainer performs the
independent-review round; what helps you pass it fastest:

- **Behavior claims need evidence**: a regression test that fails
  before your change and passes after, a benchmark, or a screenshot
  for pure UI work.
- **Order-of-operations comments**: when a sequence matters (e.g.
  "land the failure before requeueing"), write why in a comment.
- **State-machine changes** must respect the transition table in
  `crates/api/src/task.rs` (`can_transition`) — no new edges without
  an explicit design note.

## Design & review records

Substantial features start with a design note in `docs/design/` and
land with a review record in `docs/reviews/`. Look at existing entries
for the expected depth — they are the project's institutional memory
and the reason subtle regressions (resume accounting, CoalescingSink
ordering, bits-ui layer wedges) get caught before review.

## Commit & PR style

- Conventional commits (`feat:`, `fix:`, `perf:`, `docs:`, `chore:`).
- One logical change per PR. CI must be green
  (`fmt` + `clippy -D warnings` + workspace tests + frontend gates).
- UI changes: attach before/after screenshots.
- Engine/scheduler changes: include or update the scripted-port tests
  in `crates/scheduler/tests/`.

## Reporting bugs

Open a GitHub issue — see the bug template. Attach `peregrined` logs
(`--tcp` surface logs to stderr) and the task row (`GET /tasks/{id}`)
when relevant.
