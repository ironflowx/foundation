#!/usr/bin/env bash
# Usage: ci/release-archive.sh <target> (after the native release build in release.yml).
set -euo pipefail
cd "$(dirname "$0")/.."

target=${1:?target triple required}
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml)
archive=ironflowx-$version-$target.tar.gz
binary=${CARGO_TARGET_DIR:-target}/$target/release/ironflowx
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

mkdir -p dist "$work/stage" "$work/extracted"
install -m 755 "$binary" "$work/stage/ironflowx"
install -m 644 LICENSE "$work/stage/LICENSE"
COPYFILE_DISABLE=1 tar -czf "dist/$archive" -C "$work/stage" ironflowx LICENSE
(
  cd dist
  shasum -a 256 "$archive" >"$archive.sha256"
  shasum -a 256 -c "$archive.sha256"
)

members=$(tar -tzf "dist/$archive")
if [ "$members" != "$(printf '%s\n' ironflowx LICENSE)" ]; then
  echo "unexpected archive members: $members" >&2
  exit 1
fi
tar -xzf "dist/$archive" -C "$work/extracted"
cmp LICENSE "$work/extracted/LICENSE"
file "$work/extracted/ironflowx"
if [[ "$target" == *-linux-musl ]]; then
  elf=$(readelf -lW -dW "$work/extracted/ironflowx")
  if grep -Eq '^[[:space:]]*INTERP[[:space:]]|\(NEEDED\)' <<<"$elf"; then
    echo "archive's Linux binary needs a dynamic loader or library" >&2
    exit 1
  fi
fi
"$work/extracted/ironflowx" version | jq -e --arg version "$version" '.version == $version'
python3 ci/release-https.py "$work/extracted/ironflowx" "$work"
