#!/usr/bin/env bash
# Usage: ci/must-reject.sh <pattern> <command...>
# Proves a check still works: the command must fail, and its output must contain <pattern>, so a
# fixture that fails for an unrelated reason (a typo, a missing tool) does not count.
set -uo pipefail
pattern=$1
shift
output=$("$@" 2>&1)
code=$?
if [ "$code" -eq 0 ]; then
  echo "$output"
  echo "must-reject: '$*' passed, but it must reject its fixture"
  exit 1
fi
if ! grep -qF -- "$pattern" <<<"$output"; then
  echo "$output"
  echo "must-reject: '$*' failed without '$pattern'"
  exit 1
fi
echo "must-reject: '$*' rejected its fixture as expected"
