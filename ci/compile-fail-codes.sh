#!/usr/bin/env bash
# Usage: ci/compile-fail-codes.sh <path>...
# Fails if a `compile_fail` doctest under <path>, in Rust or in Markdown (a crate can include one),
# names no error code: it passes on any failure to compile, a typo included. That the code is the
# right one, ci/public-api.sh checks on nightly.
set -euo pipefail
found=$(grep -rnE --include='*.rs' --include='*.md' '(```|~~~).*compile_fail' "$@" | grep -vwE 'E[0-9]{4}' || true)
if [ -n "$found" ]; then
  echo "compile_fail doctests that name no error code:"
  echo "$found"
  exit 1
fi
