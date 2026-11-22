#!/usr/bin/env bash
# Usage: ci/no-key-attribution.sh <dir>...
# Fails if Rust code under <dir> calls a `key` associated function (`SubmitAs::key`, an alias of
# it, `Self::key`) outside the credentials crate (ironflowx-authz) and the file that defines it
# (D17, D36). Comment lines are skipped, so docs may name the constructor.
# Clippy covers every other crate, but inside ironflowx-core the constructor's path does not
# resolve, so core needs this grep. It matches any `::key`, not only `SubmitAs::key`, so a
# renamed import or `Self::key` in another impl block is caught too.
# shortcut: a `::key` that is not SubmitAs's fails here as well; add an exemption when one exists.
set -euo pipefail
found=$(grep -rnE --include='*.rs' '::key\b' "$@" \
  | grep -vE '^[^:]+:[0-9]+:[[:space:]]*//' \
  | grep -vE '^(\./)?crates/ironflowx-authz/' \
  | grep -vE '^(\./)?crates/ironflowx-core/src/who\.rs:' || true)
if [ -n "$found" ]; then
  echo "SubmitAs::key outside ironflowx-authz:"
  echo "$found"
  exit 1
fi
