#!/usr/bin/env bash
# Usage: ci/required-checks.sh
# The `main` ruleset requires every job of ci.yml but `soak`, and the check still rejects its
# fixture. Reads the live ruleset, so it needs `gh`, logged in.
set -euo pipefail
cd "$(dirname "$0")/.."
gh api repos/ironflowx/foundation/rules/branches/main |
  ci/check-required.sh .github/workflows/ci.yml soak
# shellcheck disable=SC2016 # the inner shell expands it
ci/must-reject.sh "not required: forgotten" "not required: split (b)" "not required: pair (a, d)" \
  "required, but no job of it: gone" "required, but no job of it: by-hand" \
  "cannot tell the checks of: included" "cannot tell the checks of: excluded" \
  "cannot tell the checks of: computed" \
  "cannot tell the checks of: reused" "cannot tell the checks of: titled" \
  "cannot tell the checks of: named" "can pass without passing: skipped" \
  "can pass without passing: let-go" \
  "no job to exempt: renamed" -- \
  sh -c 'ci/check-required.sh "$0/ci.yml" by-hand renamed <"$0/rules.json"' ci/fixtures/required-checks
