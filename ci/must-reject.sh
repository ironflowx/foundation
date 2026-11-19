#!/usr/bin/env bash
# Usage: ci/must-reject.sh <pattern>... -- <command...>
# Proves a check still works: the command must fail, and its output must contain every pattern,
# so a fixture that fails for an unrelated reason (a typo, a missing tool) does not count, and
# one fixture can prove several bans in a single run.
set -uo pipefail
patterns=()
while [ $# -gt 0 ] && [ "$1" != "--" ]; do
  patterns+=("$1")
  shift
done
if [ $# -lt 2 ] || [ ${#patterns[@]} -eq 0 ]; then
  echo "usage: ci/must-reject.sh <pattern>... -- <command...>"
  exit 2
fi
shift
output=$("$@" 2>&1)
code=$?
if [ "$code" -eq 0 ]; then
  echo "$output"
  echo "must-reject: '$*' passed, but it must reject its fixture"
  exit 1
fi
missing=0
for pattern in "${patterns[@]}"; do
  if ! grep -qF -- "$pattern" <<<"$output"; then
    echo "must-reject: '$*' failed without '$pattern'"
    missing=1
  fi
done
if [ "$missing" -ne 0 ]; then
  echo "$output"
  exit 1
fi
echo "must-reject: '$*' rejected its fixture with all ${#patterns[@]} expected findings"
