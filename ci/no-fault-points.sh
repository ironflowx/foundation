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
  # By name, not path: cargo reports a symlinked path unresolved.
  name=$(cargo tree --locked --manifest-path "$manifest" --depth 0 --prefix none --format '{p}' |
    awk '{ print $1 }')
  # Cargo's own `publish`, workspace inheritance included: `[]` is unpublished. `keys` fails on a
  # crate jq cannot find, so a failed lookup stops the check instead of skipping it.
  meta=$(cargo metadata --locked --no-deps --format-version 1 --manifest-path "$manifest" |
    jq -er --arg name "$name" '[.packages[] | select(.name == $name)][0]
      | [(.publish == []), (.features | has("__fault-points")),
         (.features | keys | map(select(. != "__fault-points")) | join(",") | "[\(.)]")]
      | map(tostring) | join(" ")')
  read -r unpublished own features <<<"$meta"
  [ "$unpublished" = true ] && continue
  features=${features:1:${#features}-2}

  # Each package `cargo tree` builds with `__fault-points` on, under the given features.
  hooked() {
    cargo tree --locked --manifest-path "$manifest" -e normal,build --target all --prefix none \
      --format '{p} [{f}]' "$@" | awk '/[[,]__fault-points[],]/ { print $1 }' | sort -u
  }
  # Every feature but the crate's own `__fault-points`, defaults included: any hook on, the
  # crate's own included, came another way.
  other=$(hooked --features "$features")
  # Every feature against defaults plus its own `__fault-points`: a hook on only in the first
  # needed another feature, such as one that turns on a hook of a dependency only
  # `__fault-points` pulls in.
  # shortcut: also flags a hook that `__fault-points` turns on, through `dep?/__fault-points`, on a
  # dependency only another feature pulls in; fails closed. Upgrade if a crate needs that.
  all=$(hooked --all-features)
  if [ "$own" = true ]; then forwarded=$(hooked --features __fault-points); else forwarded=$(hooked); fi
  found=$(sort -u <(printf '%s\n' "$other") <(comm -23 <(printf '%s\n' "$all") <(printf '%s\n' "$forwarded")) | grep . || true)
  for pkg in $found; do
    echo "$manifest: published crate can enable __fault-points on $pkg"
    status=1
  done
done
exit $status
