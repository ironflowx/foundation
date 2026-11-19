#!/usr/bin/env bash
# Usage: ci/no-bare-env-id.sh <public-api snapshot...>
# Fails if a public function takes an EnvironmentId without its token (LED-9). Methods of
# EnvironmentId itself (eq, fmt, hash) are its own trait impls, not lookups.
set -euo pipefail
found=$(grep -hE '^pub (const |async |unsafe )*fn ' "$@" \
  | grep -v 'fn ironflowx_core::EnvironmentId::' \
  | sed 's/ -> .*//' \
  | grep -F 'EnvironmentId' || true)
if [ -n "$found" ]; then
  echo "public functions taking a bare EnvironmentId (take an EnvRef):"
  echo "$found"
  exit 1
fi
