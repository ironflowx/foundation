#!/usr/bin/env bash
# Usage: ci/no-fault-points.sh <dir>
# Crash test: fails if a published crate in <dir>/*/ can build with `__fault-points` on any
# package: by default, through a dependency, through one of its own features, or on any target.
# Dev-dependencies do not ship, so they do not count.
# `cargo tree` on each package alone, as ci/check-deps.sh does: a workspace-wide resolve would
# unify in the feature an unpublished crate (the crash test) enables, and flag every crate.
set -euo pipefail
dir=$1

status=0
for manifest in "$dir"/*/Cargo.toml; do
  # shortcut: only `publish = false` marks a crate unpublished; a registry list would read as published.
  grep -qE '^publish *= *false' "$manifest" && continue
  # Default features flag the crate itself; all features flag only its dependencies, since the
  # crate that defines `__fault-points` always has it as an opt-in feature.
  found=$({
    cargo tree --locked --manifest-path "$manifest" -e normal,build --target all --prefix none \
      --format '{p} [{f}]' | awk '/[[,]__fault-points[],]/ { print $1 }'
    cargo tree --locked --manifest-path "$manifest" -e normal,build --target all --all-features \
      --prefix none --format '{p} [{f}]' | awk 'NR > 1 && /[[,]__fault-points[],]/ { print $1 }'
  } | sort -u)
  for pkg in $found; do
    echo "$manifest: published crate can enable __fault-points on $pkg"
    status=1
  done
done
exit $status
