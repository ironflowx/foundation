#!/usr/bin/env bash
# Usage: ci/no-engine-deps.sh <Cargo.toml...>
# SRV-1: fails if a package at <Cargo.toml> depends, directly or not, with any feature and on any
# target, on an ironflowx crate other than ironflowx-core and ironflowx-proto: a door holds no rule,
# so it links no engine crate, nor the facade, which re-exports them. Dev-dependencies do not ship.
# Not caught, and not in this repo: an engine crate under another name, or its code copied in.
set -euo pipefail
status=0
for manifest in "$@"; do
  # First line is the package itself.
  deps=$(cargo tree --locked --manifest-path "$manifest" -e normal,build --all-features \
    --target all --prefix none --format '{p}' | awk 'NR > 1 { print $1 }' | sort -u)
  for dep in $deps; do
    case $dep in
      ironflowx-core | ironflowx-proto) ;;
      ironflowx | ironflowx-*)
        echo "$manifest: engine crate: '$dep'"
        status=1
        ;;
    esac
  done
done
exit $status
