#!/usr/bin/env bash
# Usage: ci/no-digest-codec.sh <dir>...
# Fails if Rust code under <dir> calls the key digest's storage codec, `to_stored` or
# `from_stored`, outside the control plane (ironflowx-store) and the file that defines it
# (TEN-4, D17's pattern). Comment lines are skipped.
# Clippy covers every crate, authz included; this grep backs it up by name (D17's pattern), so a
# lint config that loses the ban still fails here. It matches the bare names, so an alias is
# caught too.
set -euo pipefail
found=$(grep -rnE --include='*.rs' '\b(to|from)_stored\b' "$@" \
  | grep -vE '^[^:]+:[0-9]+:[[:space:]]*//' \
  | grep -vE '^(\./)?crates/ironflowx-store/' \
  | grep -vE '^(\./)?crates/ironflowx-authz/src/digest\.rs:' || true)
if [ -n "$found" ]; then
  echo "the key digest codec outside ironflowx-store:"
  echo "$found"
  exit 1
fi
