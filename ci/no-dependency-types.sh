#!/usr/bin/env bash
# Usage: ci/no-dependency-types.sh <public-api snapshot...>
# Fails if a public API names a type from the ledger's dependencies: the log's types stay private
# to ironflowx-cell (architecture.md, "Depends on"), and neither the read store nor the runtime
# may leak through a signature or a trait impl, where a version bump becomes a breaking change.
set -euo pipefail
found=$(grep -nE '\b(rusqlite|libsqlite3_sys|segment_log_core|tokio|uuid|getrandom)::' "$@" || true)
if [ -n "$found" ]; then
  echo "public API naming a dependency's type (wrap it in a private type):"
  echo "$found"
  exit 1
fi
