#!/usr/bin/env bash
# Usage: ci/no-key-attribution.sh <dir>...
# Fails if Rust source under <dir> names SubmitAs::key outside the credentials crate
# (ironflowx-authz) and the file that defines it (D17, D36).
# Clippy covers every other crate, but inside ironflowx-core the constructor's path does not
# resolve, so core needs this grep.
set -euo pipefail
found=$(grep -rn --include='*.rs' 'SubmitAs::key' "$@" \
  | grep -vE '^(\./)?crates/ironflowx-authz/' \
  | grep -vE '^(\./)?crates/ironflowx-core/src/who\.rs:' || true)
if [ -n "$found" ]; then
  echo "SubmitAs::key outside ironflowx-authz:"
  echo "$found"
  exit 1
fi
