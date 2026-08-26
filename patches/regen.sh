#!/usr/bin/env bash
# Regenerate the vendored gpui delta. Run from the repo root.
set -euo pipefail

VERSION=0.2.2
OUT="patches/gpui-${VERSION}-zshot-overlay.patch"

PRISTINE=$(echo "$HOME"/.cargo/registry/src/*/gpui-${VERSION})
if [ ! -d "$PRISTINE" ]; then
    echo "pristine gpui ${VERSION} not in the cargo registry cache; run 'cargo fetch' first" >&2
    exit 1
fi

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
cp -r "$PRISTINE" "$WORK/a"
cp -r third_party/gpui "$WORK/b"
chmod -R u+w "$WORK"

# diff exits 1 when there are differences, which is the normal case here.
( cd "$WORK" && diff -ruN --strip-trailing-cr \
    -x Cargo.lock -x .cargo-ok -x .cargo_vcs_info.json -x Cargo.toml.orig \
    -x examples -x UPSTREAM.md \
    a b ) > "$OUT" || true

echo "wrote $OUT ($(grep -c '^+' "$OUT") added, $(grep -c '^-' "$OUT") removed)"
