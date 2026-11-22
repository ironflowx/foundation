#!/usr/bin/env bash
# Usage: ci/red.sh <file> <sed-expr> <pattern> -- <command...>
# Records a test check landing red (CODING_STANDARDS.md, "Test checks land red"): applies the
# one-line defect <sed-expr> to <file>, runs <command>, and passes only if it fails with
# <pattern> in its output (a test's name beside `panicked`, say), so a compile error or an
# unrelated failure does not count. The file is always restored from a copy, never through git,
# so uncommitted edits survive. Prints a row for the PR's red-record table.
set -uo pipefail
if [ $# -lt 5 ] || [ "$4" != "--" ]; then
  echo "usage: ci/red.sh <file> <sed-expr> <pattern> -- <command...>"
  exit 2
fi
file=$1 expr=$2 pattern=$3
shift 4
backup=$(mktemp)
cp -p "$file" "$backup" || exit 2
# Restored without -p: a fresh mtime, so cargo rebuilds instead of keeping the mutated build.
trap 'cp "$backup" "$file"; rm -f "$backup"' EXIT
sed "$expr" "$backup" >"$file" || exit 2
if cmp -s "$file" "$backup"; then
  echo "red: '$expr' changed nothing in $file"
  exit 1
fi
output=$("$@" 2>&1)
code=$?
if [ "$code" -eq 0 ]; then
  echo "red: '$*' still passes with '$expr' in $file"
  exit 1
fi
if ! grep -qF -- "$pattern" <<<"$output"; then
  echo "$output"
  echo "red: '$*' failed without '$pattern', so not for the defect"
  exit 1
fi
echo "| \`$pattern\` | \`$file\`: \`$expr\` | red |"
