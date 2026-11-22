#!/usr/bin/env bash
# Usage: ci/purity.sh
# LED-15, D17 and D36: the pure crates' dependency allowlists, the SequencedTime constructor
# outside the ledger, SubmitAs::key outside the credentials crate; the crash test's fault points in no published build; and each check rejecting its
# fixture. The clippy job enforces the call bans on the real crates.
set -euo pipefail
cd "$(dirname "$0")/.."

ci/check-deps.sh crates/ironflowx-core/Cargo.toml ci/purity/ironflowx-core.allow
ci/check-deps.sh crates/ironflowx-events/Cargo.toml ci/purity/ironflowx-events.allow
ci/no-event-clock-outside-ledger.sh crates
ci/no-key-attribution.sh crates
ci/no-fault-points.sh crates
# The allowed route passes, and was checked: a skipped crate would pass too.
forwarded=$(ci/no-fault-points.sh ci/fixtures/fault-points-forwarded)
grep -qF "checked: ci/fixtures/fault-points-forwarded/forwards/Cargo.toml" <<<"$forwarded" || {
  echo "$forwarded"
  echo "purity: ci/fixtures/fault-points-forwarded/forwards was not checked"
  exit 1
}

# The patterns quote clippy's messages, backticks included, so they stay in single quotes.
# shellcheck disable=SC2016
{
  ci/must-reject.sh "dependency not allowed: getrandom" -- \
    ci/check-deps.sh ci/fixtures/forbidden-dep/Cargo.toml ci/purity/ironflowx-core.allow
  ci/must-reject.sh \
    'disallowed method `std::time::SystemTime::now`' \
    'disallowed method `std::fs::read`' \
    'disallowed method `std::path::Path::exists`' \
    'disallowed type `std::net::TcpStream`' \
    'disallowed method `std::thread::spawn`' \
    'disallowed method `getrandom::u64`' \
    'disallowed type `std::collections::HashMap`' \
    'use of `println!`' -- \
    cargo clippy --locked --manifest-path ci/fixtures/forbidden-call/Cargo.toml -- -D warnings
  ci/must-reject.sh "incompatible with previous forbid" -- \
    cargo clippy --locked --manifest-path ci/fixtures/allow-past-ban/Cargo.toml -- -D warnings
  ci/must-reject.sh 'disallowed method `ironflowx_core::SequencedTime::from_event_clock`' -- \
    cargo clippy --locked --manifest-path ci/fixtures/sequenced-time-outside-ledger/Cargo.toml -- -D warnings
  ci/must-reject.sh "ci/fixtures/sequenced-time-outside-ledger/src/lib.rs" -- \
    ci/no-event-clock-outside-ledger.sh ci/fixtures/sequenced-time-outside-ledger
  ci/must-reject.sh 'disallowed method `ironflowx_core::SubmitAs::key`' -- \
    cargo clippy --locked --manifest-path ci/fixtures/key-attribution-outside-authz/Cargo.toml -- -D warnings
  ci/must-reject.sh "ci/fixtures/key-attribution-outside-authz/src/lib.rs" -- \
    ci/no-key-attribution.sh ci/fixtures/key-attribution-outside-authz
  ci/must-reject.sh \
    "on-by-default/Cargo.toml: published crate can enable __fault-points on on-by-default" \
    "through-dependency/Cargo.toml: published crate can enable __fault-points on ironflowx-cell" \
    "opt-in/Cargo.toml: published crate can enable __fault-points on ironflowx-cell" \
    "default-forwards/Cargo.toml: published crate can enable __fault-points on ironflowx-cell" \
    "own-feature/Cargo.toml: published crate can enable __fault-points on own-feature" \
    "implicit-optional/Cargo.toml: published crate can enable __fault-points on ironflowx-cell" \
    "metadata-publish/Cargo.toml: published crate can enable __fault-points on ironflowx-cell" \
    "weak-debug/Cargo.toml: published crate can enable __fault-points on ironflowx-cell" \
    "target-only/Cargo.toml: published crate can enable __fault-points on ironflowx-cell" -- \
    ci/no-fault-points.sh ci/fixtures/fault-points
}
