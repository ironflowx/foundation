#!/usr/bin/env bash
# Usage: ci/check-deps.sh <Cargo.toml> <allowlist>
# Fails if the package at <Cargo.toml> depends, directly or not, on a crate the allowlist does
# not name (LED-15). Dev-dependencies do not count.
# `cargo tree` on that one package, not `cargo metadata`: metadata unifies features across the
# workspace, so a feature another crate enables (uuid's v7 in the ledger) would leak in here.
set -euo pipefail
manifest=$1
allowlist=$2

# First line is the package itself.
deps=$(cargo tree --locked --manifest-path "$manifest" -e normal,build --prefix none --format '{p}' \
  | awk 'NR > 1 { print $1 }' | sort -u)

status=0
for dep in $deps; do
  if ! grep -qxF "$dep" "$allowlist"; then
    echo "$manifest: dependency not allowed: $dep (allowlist: $allowlist)"
    status=1
  fi
done
exit $status
