#!/usr/bin/env bash
# Usage: ci/check.sh
# Runs every CI job but `soak` locally, in .github/workflows/ci.yml's order, and stops at the
# first failure.
# The tools come from ci/install-tools.sh; actionlint, shellcheck and gh (logged in) from your
# package manager.
# Commit first: public-api compares the snapshots with git.
set -euo pipefail
cd "$(dirname "$0")/.."
# Each step's output is shown only if it fails, so a passing run stays a few lines long.
step() {
  echo "== $*"
  local out
  out=$(mktemp)
  if ! "$@" >"$out" 2>&1; then
    cat "$out"
    rm -f "$out"
    exit 1
  fi
  rm -f "$out"
}
step cargo fmt --all --check
step sh -c 'ci/comment-width.sh crates examples ci/*.sh'
step ci/must-reject.sh "ci/fixtures/comment-width/long.sh:3:" "ci/fixtures/comment-width/long.rs:3:" \
  "ci/fixtures/comment-width/long.rs:4:" "ci/fixtures/comment-width/long.rs:5: 101 columns" -- \
  ci/comment-width.sh ci/fixtures/comment-width
step ci/comment-width.sh ci/fixtures/comment-width-ok
step cargo clippy --workspace --all-targets --locked -- -D warnings
step ci/no-protoc.sh cargo build --workspace --all-targets --locked
needs=ci/fixtures/needs-protoc
with_protoc() {
  local in=$PWD/$needs
  PATH="$in/has-protoc:$in/has-buf:$PATH" PROTOC="$in/has-protoc/protoc" \
    BUF="$in/has-buf/buf" "$@"
}
step with_protoc cargo build --locked --manifest-path "$needs/Cargo.toml"
step with_protoc ci/must-reject.sh "protoc did not run" "buf did not run" -- \
  ci/no-protoc.sh cargo build --locked --manifest-path "$needs/Cargo.toml"
# CI's crash-test budget, whatever a replay left in the shell.
step env -u CRASH_SEED CRASH_KILLS=100 cargo test --workspace --locked
step env NO_CLIENT_TLS=1 cargo test -p ironflowx-proto --locked --test client
step cargo test -p ironflowx-proto --locked --test client --features client-tls,rustls/ring
step cargo test -p ironflowx-events -p ironflowx-cli --locked --features serde_json/arbitrary_precision
step cargo deny check
step ci/purity.sh
step sh -c 'cd examples/embed && cargo fmt --check && cargo clippy --locked -- -D warnings && cargo run --locked'
step ci/public-api.sh
step ci/doc-pins.sh
step actionlint
step shellcheck -x ci/*.sh .githooks/*
step ci/required-checks.sh
echo "== all CI checks passed"
