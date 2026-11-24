#!/usr/bin/env bash
# Usage: ci/compile-fail-codes.sh <dir>...
# Fails if a `compile_fail` doctest under <dir> names no error code: it passes on any failure to
# compile, a typo included. That the code is the right one, ci/public-api.sh checks on nightly.
set -euo pipefail
found=$(grep -rnE --include='*.rs' '(```|~~~).*compile_fail' "$@" | grep -vE 'E[0-9]{4}' || true)
if [ -n "$found" ]; then
  echo "compile_fail doctests that name no error code:"
  echo "$found"
  exit 1
fi
