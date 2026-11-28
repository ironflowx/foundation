#!/usr/bin/env bash
# Usage: ci/purity.sh
# LED-15, D17 and D36: the pure crates' dependency allowlists, the SequencedTime constructor
# outside the ledger, SubmitAs::key outside the credentials crate, Grant::submit_as outside the
# engine, engine_event outside event and execution rules (RUN-14), the key digest codec outside
# the control plane (TEN-4), every crate-wide ban in every clippy.toml; the doors' engine crates
# (SRV-1); what the generator refuses (SRV-2, SRV-12); the crash test's fault points in no
# published build; ci/red.sh, which records test checks landing red; ci/app-ref.sh, which reads
# the app branch conformance tests (D122); and each check rejecting its fixture. The clippy job
# enforces the call bans on the real crates.
set -euo pipefail
cd "$(dirname "$0")/.."

ci/check-deps.sh crates/ironflowx-core/Cargo.toml ci/purity/ironflowx-core.allow
ci/check-deps.sh crates/ironflowx-events/Cargo.toml ci/purity/ironflowx-events.allow
ci/check-deps.sh crates/ironflowx-authz/Cargo.toml ci/purity/ironflowx-authz.allow
ci/check-deps.sh crates/ironflowx-execution/Cargo.toml ci/purity/ironflowx-execution.allow
ci/no-event-clock-outside-ledger.sh crates
ci/no-key-attribution.sh crates
ci/clippy-bans.sh .
ci/no-digest-codec.sh crates
ci/no-engine-event.sh .
# Comments may name the constructor.
ci/no-key-attribution.sh ci/fixtures/key-attribution-in-comment
ci/no-engine-deps.sh crates/ironflowx-doors/Cargo.toml crates/ironflowx-proto/Cargo.toml
# A dev-dependency does not ship.
ci/no-engine-deps.sh ci/fixtures/engine-dep/allowed/Cargo.toml
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
  ci/must-reject.sh "dependency not allowed: getrandom" -- \
    ci/check-deps.sh ci/fixtures/forbidden-dep/Cargo.toml ci/purity/ironflowx-authz.allow
  ci/must-reject.sh "dependency not allowed: getrandom" -- \
    ci/check-deps.sh ci/fixtures/forbidden-dep/Cargo.toml ci/purity/ironflowx-execution.allow
  ci/must-reject.sh \
    'disallowed method `ironflowx_authz::KeyDigest::to_stored`' \
    'disallowed method `ironflowx_authz::KeyDigest::from_stored`' -- \
    cargo clippy --locked --manifest-path ci/fixtures/digest-codec-outside-store/Cargo.toml -- -D warnings
  ci/must-reject.sh \
    "ci/fixtures/digest-codec-outside-store/src/lib.rs:7:" \
    "ci/fixtures/digest-codec-outside-store/src/lib.rs:11:" -- \
    ci/no-digest-codec.sh ci/fixtures/digest-codec-outside-store
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
  # The same calls under execution rules' copy of the bans (LED-15, extended).
  ci/must-reject.sh \
    'disallowed method `std::time::SystemTime::now`' \
    'disallowed method `std::fs::read`' \
    'disallowed method `std::path::Path::exists`' \
    'disallowed type `std::net::TcpStream`' \
    'disallowed method `std::thread::spawn`' \
    'disallowed method `getrandom::u64`' \
    'disallowed type `std::collections::HashMap`' \
    'use of `println!`' -- \
    cargo clippy --locked --manifest-path ci/fixtures/forbidden-call-execution/Cargo.toml -- -D warnings
  ci/must-reject.sh \
    "engine-dep/direct/Cargo.toml: engine crate: 'ironflowx-operations'" \
    "engine-dep/renamed/Cargo.toml: engine crate: 'ironflowx-cell'" \
    "engine-dep/optional/Cargo.toml: engine crate: 'ironflowx-store'" \
    "engine-dep/target-only/Cargo.toml: engine crate: 'ironflowx-authz'" \
    "engine-dep/at-build/Cargo.toml: engine crate: 'ironflowx-events'" \
    "engine-dep/through/Cargo.toml: engine crate: 'ironflowx-operations'" \
    "engine-dep/facade/Cargo.toml: engine crate: 'ironflowx'" \
    "engine-dep/generator/Cargo.toml: linked at run time: 'ironflowx-codegen'" -- \
    ci/no-engine-deps.sh ci/fixtures/engine-dep/{direct,renamed,optional,target-only,at-build,through,facade,generator}/Cargo.toml
  # Each refusal fails a build of its own; the package that has no fault builds.
  cargo test -q --locked -p accepted --manifest-path ci/fixtures/codegen/Cargo.toml
  ci/must-reject.sh \
    "no_cli.EventService.Emit: has no (ironflowx.v1.cli)" \
    "no_summary.EventService.Emit: has no (ironflowx.v1.summary)" \
    "no_example.EventService.Emit: has no (ironflowx.v1.example)" \
    "bad_example.EventService.Emit: its (ironflowx.v1.example) fails its schema: " \
    "no_json_name.EmitRequest.event_name: its json_name, eventName, is not snake_case" -- \
    cargo build --locked --keep-going --workspace --manifest-path ci/fixtures/codegen/Cargo.toml
  ci/must-reject.sh "incompatible with previous forbid" -- \
    cargo clippy --locked --manifest-path ci/fixtures/allow-past-ban/Cargo.toml -- -D warnings
  ci/must-reject.sh 'disallowed method `ironflowx_core::SequencedTime::from_event_clock`' -- \
    cargo clippy --locked --manifest-path ci/fixtures/sequenced-time-outside-ledger/Cargo.toml -- -D warnings
  ci/must-reject.sh \
    "ci/fixtures/sequenced-time-outside-ledger/src/lib.rs" \
    "ci/fixtures/sequenced-time-outside-ledger/escaped/aliased.rs:6:" \
    "ci/fixtures/sequenced-time-outside-ledger/escaped/aliased.rs:11:" -- \
    ci/no-event-clock-outside-ledger.sh ci/fixtures/sequenced-time-outside-ledger
  # Short messages put each finding's place and lint in one line.
  ci/must-reject.sh \
    'src/lib.rs:9:5: error: use of a disallowed method `ironflowx_events::engine_event`' \
    'src/lib.rs:13:5: error: use of a disallowed method `ironflowx_events::engine_event`' \
    'src/lib.rs:17:28: error: use of a disallowed method `ironflowx_events::engine_event`' -- \
    cargo clippy --locked --message-format=short \
    --manifest-path ci/fixtures/engine-event-outside-execution/Cargo.toml -- -D warnings
  ci/must-reject.sh \
    "engine-event-outside-execution/src/lib.rs:5:" \
    "engine-event-outside-execution/src/lib.rs:9:" \
    "engine-event-outside-execution/src/lib.rs:17:" \
    "engine-event-outside-execution/src/lib.rs:23:" \
    "engine-event-outside-execution/src/lib.rs:29:" \
    "engine-event-outside-execution/included.rs:2:" \
    "engine-event-outside-execution/included.md:4:" -- \
    ci/no-engine-event.sh ci/fixtures/engine-event-outside-execution
  ci/must-reject.sh \
    "crates/ironflowx-core/clippy.toml: missing the ironflowx_core::SubmitAs::key ban" \
    "crates/ironflowx-cell/clippy.toml: missing the ironflowx_events::engine_event ban" \
    "crates/ironflowx-store/.clippy.toml: missing the ironflowx_events::engine_event ban" \
    "crates/ironflowx-operations/clippy.toml: missing the ironflowx_events::engine_event ban" \
    "crates/ironflowx-doors/clippy.toml: missing the ironflowx_events::engine_event ban" \
    "crates/ironflowx-proto/clippy.toml: missing the ironflowx_events::engine_event ban" \
    "crates/ironflowx-cell/clippy.toml: missing the ironflowx_core::SubmitAs::key ban" \
    "crates/ironflowx-events/clippy.toml: missing the ironflowx_core::SequencedTime::from_event_clock ban" \
    "crates/ironflowx-authz/clippy.toml: its bans differ" \
    "crates/ironflowx-execution/clippy.toml: its bans differ" -- \
    ci/clippy-bans.sh ci/fixtures/missing-ban
  ci/must-reject.sh 'disallowed method `ironflowx_core::SubmitAs::key`' -- \
    cargo clippy --locked --manifest-path ci/fixtures/key-attribution-outside-authz/Cargo.toml -- -D warnings
  ci/must-reject.sh 'disallowed method `ironflowx_authz::Grant::submit_as`' -- \
    cargo clippy --locked --manifest-path ci/fixtures/grant-attribution-outside-engine/Cargo.toml -- -D warnings
  ci/must-reject.sh \
    "ci/fixtures/key-attribution-outside-authz/src/lib.rs" \
    "ci/fixtures/key-attribution-outside-authz/escaped/aliased.rs" \
    "ci/fixtures/key-attribution-outside-authz/escaped/self_key.rs" -- \
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

# ci/no-engine-event.sh matches the name alone: the fixture's longer one passes.
# Captured first: under pipefail the check's own failure would hide the match.
events=$(ci/no-engine-event.sh ci/fixtures/engine-event-outside-execution || true)
if grep -qF count_engine_events <<<"$events"; then
  echo "purity: ci/no-engine-event.sh refuses a longer name, count_engine_events"
  exit 1
fi
# ci/clippy-bans.sh counts a ban written path="..." whose reason holds a #: the operations fixture
# carries every other ban so.
bans=$(ci/clippy-bans.sh ci/fixtures/missing-ban || true)
if grep -F "ironflowx-operations/clippy.toml" <<<"$bans" | grep -vqF "ironflowx_events::engine_event"; then
  echo "$bans"
  echo "purity: ci/clippy-bans.sh misses a ban in ci/fixtures/missing-ban/crates/ironflowx-operations"
  exit 1
fi

# ci/must-reject.sh takes a pattern in one line of the output, no fewer, no more and no other's.
ci/must-reject.sh "with 'BadType' in 0 lines" -- \
  ci/must-reject.sh BadType -- sh -c 'echo BadOther; exit 1'
ci/must-reject.sh "with 'BadType' in 2 lines" -- \
  ci/must-reject.sh BadType -- sh -c 'echo BadType; echo BadTypeHolder; exit 1'
ci/must-reject.sh "with 'BadTypeHolder' only in another pattern's line" -- \
  ci/must-reject.sh BadType BadTypeHolder -- sh -c 'echo BadTypeHolder; exit 1'
ci/must-reject.sh "passed, but it must reject" -- ci/must-reject.sh BadType -- echo BadType

# ci/red.sh passes only on a failure that names its pattern, of a command that without the defect
# passes and does not print it, and restores the file every time, with a new mtime: one it kept
# would leave cargo using the build that had the defect in.
red=ci/fixtures/red
before=$(mktemp)
sleep 1
ci/red.sh "$red/value.txt" 's/ok/no/' "'value_is_ok' panicked" -- "$red/check.sh" >/dev/null
[ "$red/value.txt" -nt "$before" ] || {
  echo "purity: ci/red.sh restored $red/value.txt with its old mtime"
  exit 1
}
rm -f "$before"
ci/must-reject.sh "changed nothing" -- ci/red.sh "$red/value.txt" 's/absent/no/' x -- "$red/check.sh"
ci/must-reject.sh "still passes" -- ci/red.sh "$red/value.txt" 's/ok/OK/' x -- true
ci/must-reject.sh "failed without 'other_test'" -- \
  ci/red.sh "$red/value.txt" 's/ok/no/' other_test -- "$red/check.sh"
ci/must-reject.sh "a passing run prints 'value_is_ok'" -- \
  ci/red.sh "$red/value.txt" 's/ok/no/' value_is_ok -- "$red/check.sh"
ci/must-reject.sh "fails without the defect" -- \
  ci/red.sh "$red/value.txt" 's/ok/no/' x -- sh -c 'echo x; exit 1'
grep -qx ok "$red/value.txt" || {
  echo "purity: ci/red.sh did not restore $red/value.txt"
  exit 1
}

# ci/app-ref.sh: one fixture per refusal, and a branch read as its full ref (D122).
app=ci/fixtures/app-ref
ci/must-reject.sh "$app/missing is missing" -- ci/app-ref.sh "$app/missing"
for case in empty newline-only; do
  ci/must-reject.sh "$app/$case is empty" -- ci/app-ref.sh "$app/$case"
done
for case in two-lines two-lines-no-newline blank-line; do
  ci/must-reject.sh "$app/$case has 2 lines" -- ci/app-ref.sh "$app/$case"
done
for case in crlf trailing-space bom nul latin1; do
  ci/must-reject.sh "$app/$case holds a byte other than" -- ci/app-ref.sh "$app/$case"
done
ci/must-reject.sh "$app/dots is not a name git takes for a branch: 'main..other'" -- ci/app-ref.sh "$app/dots"
# The full ref, of a branch that is not main and holds every kind of byte allowed; the real file
# may name any branch (a paired PR's).
# Assigned first, so the reader's exit status counts.
read_ref=$(ci/app-ref.sh "$app/pair-branch") || {
  echo "purity: ci/app-ref.sh failed on $app/pair-branch"
  exit 1
}
[ "$read_ref" = refs/heads/pair/Runs-214_a.b ] || {
  echo "purity: ci/app-ref.sh does not read $app/pair-branch as refs/heads/pair/Runs-214_a.b"
  exit 1
}
read_ref=$(ci/app-ref.sh) || {
  echo "purity: ci/app-ref.sh failed on ci/app-ref"
  exit 1
}
[ "$read_ref" = "refs/heads/$(cat ci/app-ref)" ] || {
  echo "purity: ci/app-ref.sh does not read ci/app-ref"
  exit 1
}
