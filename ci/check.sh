#!/usr/bin/env bash
# Usage: ci/check.sh
# Runs every CI job locally, in .github/workflows/ci.yml's order, and stops at the first failure.
# The tools come from ci/install-tools.sh; actionlint and shellcheck from your package manager.
# Commit first: public-api compares the snapshots with git.
set -euo pipefail
cd "$(dirname "$0")/.."
step() { echo "== $*"; "$@"; }
step cargo fmt --all --check
step cargo clippy --workspace --all-targets --locked -- -D warnings
step cargo test --workspace --locked
step cargo test -p ironflowx-events --locked --features serde_json/arbitrary_precision
step cargo deny check
step ci/purity.sh
step ci/public-api.sh
step actionlint
step shellcheck -x ci/*.sh .githooks/*
echo "== all CI checks passed"
