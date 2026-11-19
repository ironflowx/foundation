#!/usr/bin/env bash
# Usage: ci/check-deps.sh <Cargo.toml> <allowlist>
# Fails if the package at <Cargo.toml> depends, directly or not, on a crate the allowlist does
# not name. Dev-dependencies do not count. Reads `cargo metadata` for the host platform (LED-15).
set -euo pipefail
manifest=$(cd "$(dirname "$1")" && pwd -P)/$(basename "$1")
allowlist=$2
host=$(rustc -vV | sed -n 's/^host: //p')

metadata=$(cargo metadata --format-version 1 --locked --manifest-path "$manifest" --filter-platform "$host")
deps=$(jq -r --arg manifest "$manifest" '
  (.packages | map({key: .id, value: .name}) | from_entries) as $names
  | (.resolve.nodes | map({key: .id, value: .}) | from_entries) as $nodes
  | (.packages[] | select(.manifest_path == $manifest) | .id) as $root
  | [$root | recurse($nodes[.].deps[] | select(any(.dep_kinds[]; .kind != "dev")) | .pkg)]
  | unique - [$root] | map($names[.]) | .[]' <<<"$metadata")

status=0
for dep in $deps; do
  if ! grep -qxF "$dep" "$allowlist"; then
    echo "$manifest: dependency not allowed: $dep (allowlist: $allowlist)"
    status=1
  fi
done
exit $status
