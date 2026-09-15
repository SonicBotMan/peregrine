#!/usr/bin/env bash
# M6-a: build release artifacts for peregrine (headless).
#
#   ./scripts/package.sh            # tarball only (needs `strip`
#                                    from binutils, fail-loud if absent)
#   ./scripts/package.sh deb        # tarball + .deb (requires cargo-deb)
#
# Artifacts land in dist/:
#   peregrine-<ver>-<target>.tar.gz   — stripped pg + peregrined +
#                                      peregrine-mcp + README + LICENSE
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
# The thin client alone cannot run without its daemon: the
# tarball ships the full headless set (P1 packaging fix — alpha.2
# shipped `pg` only, which is unusable standalone).
cp target/release/pg target/release/peregrined target/release/peregrine-mcp "${OUT}/"
cp README.md LICENSE "${OUT}/"
strip "${OUT}/pg" "${OUT}/peregrined" "${OUT}/peregrine-mcp"

echo "==> tarball"
mkdir -p dist
# Reproducible archive (R2 P2): stable ownership, ordering, and
# mtime pinned to the commit timestamp; gzip -n drops the embedded
# timestamp. Toolchain differences remain (CI vs local), but the
# same commit + toolchain now yields byte-identical archives.
tar --sort=name --owner=0 --group=0 --numeric-owner \
    --mtime="@$(git log -1 --format=%ct)" \
    -c -C dist "$(basename "${OUT}")" \
    | gzip -n > "${OUT}.tar.gz"
# Bare filename in the checksum so `sha256sum -c` works wherever
# the pair is downloaded (alpha.2 emitted a `dist/` prefix).
(cd dist && sha256sum "$(basename "${OUT}").tar.gz" > "$(basename "${OUT}").tar.gz.sha256")

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
