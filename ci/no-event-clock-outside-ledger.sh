#!/usr/bin/env bash
# Usage: ci/no-event-clock-outside-ledger.sh <dir>...
# Fails if Rust source under <dir> names SequencedTime::from_event_clock outside the ledger crate,
# the file that defines it, and one use in each of the tests/rules.rs of ironflowx-events and
# ironflowx-execution, whose one helper each stands in for the ledger (D17). Run it on `crates`
# (as `ci/purity.sh` does) for the exempt files to be counted.
# Clippy covers every other crate, but inside ironflowx-core the constructor's path does not
# resolve, so core needs this grep.
set -euo pipefail
found=$(grep -rn --include='*.rs' 'from_event_clock' "$@" \
  | grep -vE '^(\./)?crates/ironflowx-cell/' \
  | grep -vE '^(\./)?crates/ironflowx-core/src/time\.rs:' \
  | grep -vE '^(\./)?crates/ironflowx-(events|execution)/tests/rules\.rs:' || true)
# Each exempt file holds its one helper's call, and no second.
for exempt in crates/ironflowx-events/tests/rules.rs crates/ironflowx-execution/tests/rules.rs; do
  for dir in "$@"; do
    file=${dir%/}/${exempt#crates/}
    [ "${dir%/}" = crates ] || [ "${dir%/}" = ./crates ] || continue
    [ -e "$file" ] || continue
    uses=$(grep -o 'from_event_clock' "$file" | wc -l | tr -d ' ')
    if [ "$uses" -gt 1 ]; then
      found+="${found:+$'\n'}$file: $uses uses, beyond its one helper"
    fi
  done
done
if [ -n "$found" ]; then
  echo "SequencedTime::from_event_clock outside ironflowx-cell:"
  echo "$found"
  exit 1
fi
