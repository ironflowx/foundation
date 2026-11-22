#!/usr/bin/env bash
# Usage: ci/clippy-bans.sh <repo-root>
# Fails if a clippy.toml under <repo-root> lacks a crate-wide ban it must carry. Clippy configs
# cannot include one another, so each ban is copied into the root file and every crate's own
# file, and the must-reject fixtures exercise only the root one (D17, D36).
set -euo pipefail
root=$1
# <path>|<crate exempt from it>
bans=(
  "ironflowx_core::SequencedTime::from_event_clock|ironflowx-cell"
  "ironflowx_core::SubmitAs::key|ironflowx-authz"
)
missing=0
for config in "$root/clippy.toml" "$root"/crates/*/clippy.toml; do
  [ -e "$config" ] || continue
  crate=$(basename "$(dirname "$config")")
  for ban in "${bans[@]}"; do
    path=${ban%|*} exempt=${ban#*|}
    [ "$crate" = "$exempt" ] && continue
    if ! grep -qF "path = \"$path\"" "$config"; then
      echo "$config: missing the $path ban"
      missing=1
    fi
  done
done
exit "$missing"
