#!/usr/bin/env bash
# Usage: ci/clippy-bans.sh <repo-root>
# Fails if a clippy.toml or .clippy.toml under <repo-root> lacks a crate-wide ban it must carry,
# outside a comment, whole-line or trailing. Clippy configs cannot include one another, so each
# ban is copied into the root file and every crate's own file, and the must-reject fixtures
# exercise only the root one (D17, D36, TEN-4, RUN-14).
# Not caught: an #[allow(clippy::disallowed_methods)] gets past every ban in a crate whose root
# does not forbid the lint, and in any integration test; a by-name grep backs each ban up
# (ci/no-event-clock-outside-ledger.sh, no-key-attribution.sh, no-digest-codec.sh,
# no-engine-event.sh). This repo's only suppressions are the event-rules test helpers that stand
# in for the ledger and for execution rules (crates/ironflowx-events/tests/rules.rs). Nor is a
# ban under another key than disallowed-methods, which a line-by-line read cannot tell; every
# file here lists it under disallowed-methods. Nor is a clippy.toml outside the root and crates/*
# (examples/embed has none, and takes the root's), or a manifest's [lints.clippy] allowing the
# lint (the one [lints] here, ironflowx-cell's, only warns of two casts; the by-name greps catch
# what it lets through). Wrongly refused: a ban after a literal string that
# holds a # on its line, or one split over lines, or in a multi-line string; no file here has
# one.
set -euo pipefail
root=$1
# <path>|<crate exempt from it>
bans=(
  "ironflowx_core::SequencedTime::from_event_clock|ironflowx-cell"
  "ironflowx_core::SubmitAs::key|ironflowx-authz"
  "ironflowx_authz::Grant::submit_as|ironflowx-operations"
  "ironflowx_authz::KeyDigest::to_stored|ironflowx-store"
  "ironflowx_authz::KeyDigest::from_stored|ironflowx-store"
  # ironflowx-events carries this one too, through core's file: its library cannot call
  # engine_event, and its tests' one helper expects the lint.
  "ironflowx_events::engine_event|ironflowx-execution"
)
missing=0
for config in "$root"/{,.}clippy.toml "$root"/crates/*/{,.}clippy.toml; do
  [ -e "$config" ] || continue
  crate=$(basename "$(dirname "$config")")
  for ban in "${bans[@]}"; do
    path=${ban%|*} exempt=${ban#*|}
    [ "$crate" = "$exempt" ] && continue
    # A line counts when the ban survives a cut at the first # outside a basic string, and again
    # outside a basic or a literal ('...') string.
    if ! BAN=$path perl -ne '
        my $ban = qr/\bpath\s*=\s*"\Q$ENV{BAN}\E"/;
        (my $basic = $_) =~ s/^((?:[^"#]|"(?:[^"\\]|\\.)*")*)#.*/$1/;
        (my $both = $_) =~ s/^((?:[^"#\x27]|"(?:[^"\\]|\\.)*"|\x27[^\x27]*\x27)*)#.*/$1/;
        $found = 1 if $basic =~ $ban && $both =~ $ban;
        END { exit !$found }' "$config"; then
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
