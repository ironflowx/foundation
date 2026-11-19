#!/usr/bin/env bash
# Usage: ci/no-event-clock-outside-ledger.sh <dir>...
# Fails if Rust source under <dir> names SequencedTime::from_event_clock outside the ledger crate,
# the file that defines it and ironflowx-events' tests/rules.rs, whose one helper stands in for
# the ledger (D17).
# Clippy covers every other crate, but inside ironflowx-core the constructor's path does not
# resolve, so core needs this grep.
set -euo pipefail
found=$(grep -rn --include='*.rs' 'from_event_clock' "$@" \
  | grep -vE '^(\./)?crates/ironflowx-cell/' \
  | grep -vE '^(\./)?crates/ironflowx-core/src/time\.rs:' \
  | grep -vE '^(\./)?crates/ironflowx-events/tests/rules\.rs:' || true)
if [ -n "$found" ]; then
  echo "SequencedTime::from_event_clock outside ironflowx-cell:"
  echo "$found"
  exit 1
fi
