#!/usr/bin/env bash
# Build a release artifact set for one target.
#
#   scripts/build-release.sh            # host target
#   scripts/build-release.sh <target>   # cross build (needs the target + linker)
#
# Output: dist/<version>/ with the binary (or .exe), checksums, and the SBOM-ish
# build metadata. CI uses the same script so local and published builds match.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
TARGET="${1:-$(rustc -vV | sed -n 's/^host: //p')}"
OUT="dist/${VERSION}"
mkdir -p "$OUT"

echo "==> building the console"
if command -v npm >/dev/null 2>&1; then
  ( cd frontend && npm ci --no-audit --no-fund && npm run build )
else
  echo "npm not found: the console cannot be rebuilt, reusing frontend/dist" >&2
fi

echo "==> building the binary for $TARGET"
# `rust-embed` reads frontend/dist at compile time, so this must come after the
# frontend build. `--locked` keeps dependency resolution reproducible.
cargo build --release --locked --target "$TARGET"

EXE="hejiu45-oss"
case "$TARGET" in
  *windows*) EXE="hejiu45-oss.exe" ;;
esac
SRC="target/${TARGET}/release/${EXE}"

case "$TARGET" in
  x86_64-unknown-linux-musl)  NAME="hejiu45-oss-linux-amd64" ;;
  aarch64-unknown-linux-musl) NAME="hejiu45-oss-linux-arm64" ;;
  x86_64-pc-windows-msvc)     NAME="hejiu45-oss-windows-amd64.exe" ;;
  aarch64-pc-windows-msvc)    NAME="hejiu45-oss-windows-arm64.exe" ;;
  x86_64-apple-darwin)        NAME="hejiu45-oss-darwin-amd64" ;;
  aarch64-apple-darwin)       NAME="hejiu45-oss-darwin-arm64" ;;
  *)                          NAME="hejiu45-oss-${TARGET}" ;;
esac

cp "$SRC" "$OUT/$NAME"
cp "$SRC" "${OUT}/${NAME}.unpacked"
ls -lh "$OUT/$NAME"

echo "==> static check (musl)"
if [[ "$TARGET" == *musl* ]] && command -v file >/dev/null 2>&1; then
  file "$OUT/$NAME"
fi

echo "==> writing checksums"
( cd "$OUT" && sha256sum "$NAME" > "checksums-hejiu45-oss.txt.tmp" )
cat "$OUT/checksums-hejiu45-oss.txt.tmp" >> "dist/checksums-hejiu45-oss.txt"
sort -u -k2 "dist/checksums-hejiu45-oss.txt" -o "dist/checksums-hejiu45-oss.txt"
rm -f "$OUT/checksums-hejiu45-oss.txt.tmp" "$OUT/${NAME}.unpacked"

echo "==> done: $OUT/$NAME"
