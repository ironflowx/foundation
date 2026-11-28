#!/usr/bin/env bash
# Usage: ci/app-ref.sh [file]
# Prints refs/heads/<branch> for the ironflowx/ironflow branch the `conformance` job checks out:
# the one line of `ci/app-ref` (D122). The full ref, so neither actions/checkout nor git fetch can
# take a tag of the same name. A missing file, an empty one, more than one line, a byte other than
# an ASCII letter, a digit, `.`, `_`, `-` or `/` (a space, a CR, a BOM, a NUL), or a name git
# refuses for a branch fails, naming the file. A well-formed name of no branch (a typo,
# `origin/main`, a commit) passes here and fails at the checkout, which names the ref, not the file;
# any ref whose name ends in refs/heads/<name>, such as a tag or a branch literally named so, would
# stand in for it, and ironflowx/ironflow has none.
# A branch name git takes with another byte (`+`, `@`, a non-ASCII letter) is refused on purpose:
# the app's branches have none.
set -euo pipefail
file=${1:-ci/app-ref}
fail() {
  echo "$file $1: it must hold one line, the ironflowx/ironflow branch conformance tests" \
    "(ci/README.md)" >&2
  exit 1
}
[ -f "$file" ] || fail "is missing"
# awk counts a last line without its newline, which wc -l does not.
lines=$(awk 'END { print NR }' "$file")
[ "$lines" -le 1 ] || fail "has $lines lines"
# Counted, not read: a command substitution drops a NUL. In C, a byte that is not UTF-8 is a byte.
other=$(LC_ALL=C tr -d 'A-Za-z0-9._/\n-' <"$file" | wc -c)
[ "$other" -eq 0 ] || fail "holds a byte other than a letter, a digit, '.', '_', '-' or '/'"
ref=$(head -n 1 "$file")
[ -n "$ref" ] || fail "is empty"
git check-ref-format --branch "$ref" >/dev/null 2>&1 || fail "is not a name git takes for a branch: '$ref'"
echo "refs/heads/$ref"
