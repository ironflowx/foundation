#!/usr/bin/env bash
# Usage: ci/purity.sh
# LED-15 and D17: the pure crates' dependency allowlists, the SequencedTime constructor outside
# the ledger; the crash test's fault points in no published build; and each check rejecting its
# fixture. The clippy job enforces the call bans on the
# real crates.
set -euo pipefail
cd "$(dirname "$0")/.."

ci/check-deps.sh crates/ironflowx-core/Cargo.toml ci/purity/ironflowx-core.allow
ci/check-deps.sh crates/ironflowx-events/Cargo.toml ci/purity/ironflowx-events.allow
ci/no-event-clock-outside-ledger.sh crates
ci/no-fault-points.sh crates

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
  ci/must-reject.sh \
    "on-by-default/Cargo.toml: published crate can enable __fault-points on on-by-default" \
    "through-dependency/Cargo.toml: published crate can enable __fault-points on ironflowx-cell" -- \
    ci/no-fault-points.sh ci/fixtures/fault-points
}
