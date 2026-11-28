#!/usr/bin/env bash
# Usage: ci/check-deps.sh <Cargo.toml> <allowlist>
# Fails if the package at <Cargo.toml> depends, directly or not, on a crate the allowlist does
# not name (LED-15), with all its features on, since a dependent can turn any of them on.
# Dev-dependencies do not count. It reads this machine's target and wasm32-unknown-unknown, which
# the wasm job builds. Not caught: a dependency of a third target only; `--target all` would also
# list ones no build takes (serde_core's `cfg(any())` serde), and none of the pure crates has one.
# `cargo tree` on that one package, not `cargo metadata`: metadata unifies features across the
# workspace, so a feature another crate enables (uuid's v7 in the ledger) would leak in here.
set -euo pipefail
manifest=$1
allowlist=$2

# First line is the package itself. This machine's target, and wasm32, which the wasm job builds.
tree() {
  cargo tree --locked --manifest-path "$manifest" -e normal,build --all-features --prefix none \
    --format '{p}' "$@" | awk 'NR > 1 { print $1 }'
}
deps=$( (tree && tree --target wasm32-unknown-unknown) | sort -u)

status=0
for dep in $deps; do
  if ! grep -qxF "$dep" "$allowlist"; then
    echo "$manifest: dependency not allowed: $dep (allowlist: $allowlist)"
    status=1
  fi
done
exit $status
