#!/usr/bin/env bash
# Usage: ci/no-host-engine-deps.sh <Cargo.toml>
# A host reaches the control plane, ledger, event rules and credentials through the engine.
# Check only this package's normal dependencies. Cargo metadata reports package names even when
# renamed, including optional and target-specific dependencies, without resolving transitives.
set -euo pipefail
manifest=$(cd "$(dirname "$1")" && pwd -P)/$(basename "$1")
forbidden=$(cargo metadata --locked --no-deps --format-version 1 --manifest-path "$manifest" |
  jq -r --arg manifest "$manifest" '
    (first(.packages[] | select(.manifest_path == $manifest))
      // error("package not found in \($manifest)"))
    | [.dependencies[] | select(.kind == null) | .name
        | select(test("^ironflowx-(cell|store|events|authz)$"))]
    | unique[]
  ')
if [ -n "$forbidden" ]; then
  while IFS= read -r dep; do
    echo "$1: direct engine dependency: '$dep'"
  done <<<"$forbidden"
  exit 1
fi
