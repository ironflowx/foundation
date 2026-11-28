#!/usr/bin/env bash
# Usage: ci/no-engine-event.sh <dir>...
# Fails if Rust code under <dir> names engine_event outside ironflowx-events and
# ironflowx-execution (RUN-14, D17's pattern). Plain // comment lines are skipped; doc comments
# are not, since a doctest is code.
# Clippy bans the call; this grep backs it up by name where clippy does not look: code behind a
# feature or cfg that CI's build leaves off, a doctest, and a lint config or an #[allow] that
# loses the ban. It matches the bare name, so an alias's import is caught too.
# Not caught: a name a macro builds from pieces (`paste`), which no one writes by accident. Code
# it wrongly refuses: another crate's own function or prose doc naming engine_event; this repo
# has none.
set -euo pipefail
found=$(grep -rnF --include='*.rs' 'engine_event' "$@" \
  | grep -vE '^[^:]+:[0-9]+:[[:space:]]*//([^/!]|$)' \
  | grep -vE '^(\./)?crates/ironflowx-(events|execution)/' || true)
if [ -n "$found" ]; then
  echo "engine_event outside ironflowx-events and ironflowx-execution:"
  echo "$found"
  exit 1
fi
