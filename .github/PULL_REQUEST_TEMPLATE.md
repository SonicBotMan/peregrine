<!-- Titles: conventional commit style — feat:/fix:/perf:/docs:/chore: -->

## What & why

<!-- One paragraph: the change and the problem it solves. Link the
     issue with "Fixes #N" when applicable. -->

## How it was verified

<!-- The three gates must be green locally before pushing:
     cargo fmt --all --check
     cargo clippy --workspace --all-targets --locked -- -D warnings
     cargo test --locked --workspace
     cd apps/desktop && svelte-check + vitest
-->

- [ ] `cargo fmt --all --check` clean
- [ ] `cargo clippy --workspace --all-targets --locked -- -D warnings` clean
- [ ] `cargo test --locked --workspace` green (new behavior → new test)
- [ ] frontend `svelte-check` + `vitest` green

## Behavior evidence

<!-- Regression test names, benchmark numbers, or before/after
     screenshots for UI changes. Behavior claims need evidence —
     logs beat adjectives. -->

## Review notes

<!-- Order-of-operations or state-machine subtleties the reviewer
     should read twice (see docs/reviews/ for house style). -->
