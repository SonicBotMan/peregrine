#!/usr/bin/env bash
# M6-a: build release artifacts for peregrine (headless).
#
#   ./scripts/package.sh            # tarball only (zero external tools)
#   ./scripts/package.sh deb        # tarball + .deb (requires cargo-deb)
#
# Artifacts land in dist/:
#   peregrine-<ver>-<target>.tar.gz   — stripped `pg` + README + LICENSE
#   peregrine_<ver>-1_amd64.deb       — same content as a dpkg
set -euo pipefail
cd "$(dirname "$0")/.."

VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
TARGET="$(rustc -vV | sed -n 's/^host: //p')"
OUT="dist/peregrine-${VERSION}-${TARGET}"

echo "==> building release (locked) for ${TARGET}"
cargo build --release --locked

echo "==> assembling ${OUT}"
rm -rf "${OUT}"
mkdir -p "${OUT}"
cp target/release/pg "${OUT}/pg"
cp README.md LICENSE "${OUT}/"
strip "${OUT}/pg"

echo "==> tarball"
mkdir -p dist
tar -czf "${OUT}.tar.gz" -C dist "$(basename "${OUT}")"
sha256sum "${OUT}.tar.gz" > "${OUT}.tar.gz.sha256"

if [[ "${1:-}" == "deb" ]]; then
  if ! command -v cargo-deb >/dev/null; then
    echo "cargo-deb not installed; skipping .deb (cargo install cargo-deb)" >&2
    exit 0
  fi
  echo "==> deb"
  cargo deb --locked -p peregrine-cli
fi

echo "==> done"
ls -la dist/
