#!/usr/bin/env bash
# Usage: ci/no-fault-points.sh <dir>
# Crash test: fails if a published crate in <dir>/*/ can build with `__fault-points` on any
# package: by default, through a dependency, through one of its own features, or on any target.
# The one route allowed: a crate's own `__fault-points` turning on a dependency's (D23).
# Dev-dependencies do not ship, so they do not count.
# `cargo tree` on each package alone, as ci/check-deps.sh does: a workspace-wide resolve would
# unify in the feature an unpublished crate (the crash test) enables, and flag every crate.
set -euo pipefail
dir=$1

status=0
for manifest in "$dir"/*/Cargo.toml; do
  # shortcut: only `publish = false` marks a crate unpublished; a registry list would read as published.
  grep -qE '^publish *= *false' "$manifest" && continue
  # By name, not path: cargo reports a symlinked path unresolved.
  name=$(cargo tree --locked --manifest-path "$manifest" --depth 0 --prefix none --format '{p}' |
    awk '{ print $1 }')
  # Every feature but `__fault-points`, implicit optional-dependency ones included. `keys` fails
  # on a crate it cannot find, so a failed lookup stops the check instead of skipping features.
  features=$(cargo metadata --locked --no-deps --format-version 1 --manifest-path "$manifest" |
    jq -er --arg name "$name" '[.packages[] | select(.name == $name)][0]
      | .features | keys | map(select(. != "__fault-points")) | join(",")')
  # Default features, then every other feature: any package with `__fault-points` on, the crate
  # itself included, got it some other way than from the crate's own `__fault-points`.
  found=$({
    cargo tree --locked --manifest-path "$manifest" -e normal,build --target all --prefix none \
      --format '{p} [{f}]'
    cargo tree --locked --manifest-path "$manifest" -e normal,build --target all \
      --features "$features" --prefix none --format '{p} [{f}]'
  } | awk '/[[,]__fault-points[],]/ { print $1 }' | sort -u)
  for pkg in $found; do
    echo "$manifest: published crate can enable __fault-points on $pkg"
    status=1
  done
done
exit $status
