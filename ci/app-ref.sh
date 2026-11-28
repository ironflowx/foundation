#!/usr/bin/env bash
# Usage: ci/app-ref.sh [file]
# Prints the ironflowx/ironflow ref the `conformance` job checks out: the one line of `ci/app-ref`
# (D122). A missing file, an empty one, more than one line, or a line that is no ref name (a
# trailing space, a CR) fails, naming the file.
set -euo pipefail
file=${1:-ci/app-ref}
fail() {
  echo "$file $1: it must hold one line, the ironflowx/ironflow ref conformance tests (ci/README.md)" >&2
  exit 1
}
[ -f "$file" ] || fail "is missing"
# awk counts a last line without its newline, which wc -l does not.
lines=$(awk 'END { print NR }' "$file")
[ "$lines" -le 1 ] || fail "has $lines lines"
ref=$(head -n 1 "$file")
[ -n "$ref" ] || fail "is empty"
git check-ref-format --allow-onelevel "$ref" >/dev/null 2>&1 || fail "names no ref: '$ref'"
echo "$ref"
