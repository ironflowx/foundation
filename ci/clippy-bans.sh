#!/usr/bin/env bash
# Usage: ci/clippy-bans.sh <repo-root>
# Fails if a clippy.toml or .clippy.toml under <repo-root> lacks a crate-wide ban it must carry,
# in a line that is not a comment. Clippy configs cannot include one another, so each ban is
# copied into the root file and every crate's own file, and the must-reject fixtures exercise
# only the root one (D17, D36, TEN-4, RUN-14).
# Not caught: an #[allow(clippy::disallowed_methods)] gets past every ban in a crate whose root
# does not forbid the lint, and in any integration test; a by-name grep backs each ban up
# (ci/no-event-clock-outside-ledger.sh, no-key-attribution.sh, no-digest-codec.sh,
# no-engine-event.sh). This repo's only suppressions are the event-rules test helpers that stand
# in for the ledger and for execution rules (crates/ironflowx-events/tests/rules.rs).
set -euo pipefail
root=$1
# <path>|<crate exempt from it>
bans=(
  "ironflowx_core::SequencedTime::from_event_clock|ironflowx-cell"
  "ironflowx_core::SubmitAs::key|ironflowx-authz"
  "ironflowx_authz::Grant::submit_as|ironflowx-operations"
  "ironflowx_authz::KeyDigest::to_stored|ironflowx-store"
  "ironflowx_authz::KeyDigest::from_stored|ironflowx-store"
  # ironflowx-events carries this one too, through core's file: the path does not resolve in its
  # library, and its tests' helper expects the lint.
  "ironflowx_events::engine_event|ironflowx-execution"
)
missing=0
for config in "$root"/{,.}clippy.toml "$root"/crates/*/{,.}clippy.toml; do
  [ -e "$config" ] || continue
  crate=$(basename "$(dirname "$config")")
  for ban in "${bans[@]}"; do
    path=${ban%|*} exempt=${ban#*|}
    [ "$crate" = "$exempt" ] && continue
    if ! grep -v '^[[:space:]]*#' "$config" | grep -qF "path = \"$path\""; then
      echo "$config: missing the $path ban"
      missing=1
    fi
  done
done
# authz copies core's file less the SubmitAs::key ban; every other line of the copy must match.
bans_of() { grep -v '^ *#' "$1" | grep -vF 'ironflowx_core::SubmitAs::key'; }
core=$root/crates/ironflowx-core/clippy.toml authz=$root/crates/ironflowx-authz/clippy.toml
if [ -e "$core" ] && [ -e "$authz" ] && ! diff <(bans_of "$core") <(bans_of "$authz") >/dev/null; then
  echo "$authz: its bans differ from $core's, beyond SubmitAs::key"
  missing=1
fi
exit "$missing"
