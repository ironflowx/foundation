# CI checks

`.github/workflows/ci.yml` runs these on every PR; the `main` ruleset requires each job but `soak`. Every
check script here, but for the snapshot comparison in `public-api.sh`, is proven by a fixture in
`fixtures/` that it must reject (`must-reject.sh`), so one that silently stops working fails CI.

`ci/check.sh` runs every job below but `soak` locally, in order, and stops at the first failure: run it
before you push. `git config core.hooksPath .githooks` makes `git push` run it. The root
`Makefile` names these commands; `make` lists them.

A release is `make release VERSION=X.Y.Z` (`ci/release.sh`): from a clean `main` that matches
`origin/main` with green CI, it publishes every crate without `publish = false`, then tags and
pushes `vX.Y.Z`; the tag starts `.github/workflows/release.yml`, which creates the GitHub
release from that version's `CHANGELOG.md` section, so add the section before you start. A rerun
skips what is already done. Rehearse it first with
`make release-dry-run VERSION=X.Y.Z`, which runs every check and `cargo publish --dry-run`.

| Job | Enforces | Run locally | When it fails |
|---|---|---|---|
| `fmt`, `clippy`, `clippy-macos`, `test` | Formatting, and comment lines of at most 100 columns in Rust and `ci/*.sh`, URLs exempt (`ci/comment-width.sh`: rustfmt does not wrap comments); lints with warnings denied, tests; `clippy` builds the workspace with neither `protoc` nor `buf` on `PATH` (SRV-14; `ci/no-protoc.sh`); `test` runs the event rules again with `arbitrary_precision`, the client's URL test again without `client-tls`, which the workspace's CLI switches on, and the crash test on Linux and macOS with `CRASH_KILLS=100` | `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `env -u CRASH_SEED CRASH_KILLS=100 cargo test --workspace`; event rules again with `serde_json`'s `arbitrary_precision` (D12): `cargo test -p ironflowx-events --features serde_json/arbitrary_precision`; the client's URL test without `client-tls`: `NO_CLIENT_TLS=1 cargo test -p ironflowx-proto --test client` | Fix the code |
| `deny` | Licences and advisories, Linux and macOS graphs (`deny.toml`) | `cargo deny check` | Replace the dependency, or extend `deny.toml` with a reason |
| `purity` | LED-15: pure crates' dependencies and calls; D17: only `ironflowx-cell` calls `SequencedTime::from_event_clock` (and one helper in `ironflowx-events`' `tests/rules.rs`, which stands in for the ledger); D36: no crate but the credentials crate calls `SubmitAs::key` (`ci/no-key-attribution.sh` inside core), and no crate but the engine calls `Grant::submit_as`; TEN-4: no crate but the control plane calls the key digest's storage codec (`ci/no-digest-codec.sh` backs clippy up by name); every crate-wide ban in every `clippy.toml` (`ci/clippy-bans.sh`); SRV-1: `ironflowx-doors` and `ironflowx-cli` depend on no engine crate (`ci/no-engine-deps.sh`); SRV-2 and SRV-12: a proto the generator refuses fails its build, one package of `ci/fixtures/codegen` per refusal; TEN-3: the binary built with clap's `debug` feature, which prints arguments, refuses to start (`ci/clap-trace.sh`); `ci/red.sh`, which records a test check landing red, and `ci/must-reject.sh`; crash test: no published crate (Cargo's `publish` is not `false`, set or inherited) can build with `__fault-points`: by default, through a dependency or a feature of its own, on any target; the one route allowed is a crate's own `__fault-points` forwarding to a dependency's; a dev-dependency, which does not ship, does not count (D23; `ci/no-fault-points.sh`, which needs `jq`) | `ci/purity.sh` | See [Pure crates](#pure-crates); for an engine crate, call it through `Operations`; for the generator, say what the proto leaves out; for `__fault-points`, drop it from the published crate's defaults or dependency |
| `embed` | The PRD's last "Done when": `examples/embed`, outside the workspace and on `ironflowx` and `tokio` only, bootstraps an engine, emits, reads the event back with a `read` key and sees that key's emit refused as `PermissionDenied`, on Linux and macOS. Its `.cargo/config.toml` patches `ironflowx` to this checkout; to build the published release instead, delete that file and `Cargo.lock` there, then `cargo run` | `cd examples/embed && cargo fmt --check && cargo clippy --locked -- -D warnings && cargo run --locked` | Fix the facade or the crate it names; a new dependency of the example also needs `cargo update` there |
| `public-api` | D21: each crate's public API matches `public-api/<crate>.txt`, `ironflowx-proto`'s with `client` and without `wire` and `server`; LED-9: nothing public takes a bare `EnvironmentId`; no public API names a dependency's type, but the `axum::Router` the door returns and the command line the CLI shares with the host; `ci/api-rules.sh`: nothing turns an `Address` into an ID (TEN-8), `KeyDigest` exposes nothing but `ct_eq` and its codec (TEN-4); `ci/no-secret-in-errors.sh`, over rustdoc's JSON, private and hidden items included: no error type holds a `Secret`, or is used with one (TEN-3); each `compile_fail` doctest names an error code (`ci/compile-fail-codes.sh`) and fails with it, which only nightly rustdoc checks | `ci/public-api.sh` | Review the printed diff, then `ci/public-api.sh --bless` and commit the snapshot; a new `pub use` line of ours in a snapshot also needs its name in `ci/no-dependency-types.sh` |
| `doc-pins` | D16: docs name only the `segment-log-core` version `crates/ironflowx-cell/Cargo.toml` pins (tag-pinned links) | `ci/doc-pins.sh` | Re-pin the printed links to the new tag, or drop the version from the line |
| `soak` | The crash test's longer soak before publishing: `inputs.kills` seeded kills (10000 by default) on Linux and macOS. Runs only when started by hand, with every other job, and is not required | `gh workflow run ci --ref <branch> -f kills=10000`; on your machine, `env -u CRASH_SEED CRASH_KILLS=10000 cargo test -p ironflowx-crash-test --locked --test crash seeded_random_kills` | Replay the printed `CRASH_SEED=<seed> CRASH_KILLS=1` and fix the ledger, the open or the key change it fails in |
| `lint-ci` | actionlint and shellcheck over the workflow, `ci/*.sh` and `.githooks/*`; the `main` ruleset requires exactly the jobs of `ci.yml` but `soak` (`ci/required-checks.sh`, which reads the live ruleset) | `actionlint && shellcheck -x ci/*.sh .githooks/* && ci/required-checks.sh` | Fix the script; for a job, add it to the ruleset's required checks, or remove it there |

## Pure crates

`ironflowx-core`, `ironflowx-events` and `ironflowx-authz` are pure (LED-15). Each has:

- `ci/purity/<crate>.allow`: every crate it may depend on, transitively. A new dependency of a
  pure crate goes here, with each crate it pulls in; `ci/check-deps.sh` prints what is missing.
- `clippy.toml`: core's file bans the clock, filesystem, network, threads, OS randomness and
  `HashMap`; events links to it, and authz copies it less the `SubmitAs::key` ban
  (`ci/clippy-bans.sh` checks the copy). A pure crate uses `BTreeMap`.
- The `#![forbid(...)]` header at the top of `src/lib.rs`, so an inner `#[allow]` cannot lift a ban.

A new pure crate needs all three, plus a `ci/check-deps.sh` line in `ci/purity.sh`. A new crate of
any kind needs its name in `ci/public-api.sh`, in `published` or else in the loop after it, and a
blessed snapshot.

## Fixtures

Some crates in `fixtures/` build against the workspace's crates from outside the workspace, so
`cargo` there does not show them breaking. After changing a public type or function, grep
`fixtures/` for it: `must-reject.sh` rejects a fixture that fails for a compile error instead of
its lint. Each of its patterns must be in exactly one line of the output, its own: where one name
starts another, write enough of the line to tell them apart.

## Tools

`ci/tools.env` pins every tool version; CI reads it, and `ci/install-tools.sh` installs the nightly,
cargo-public-api and cargo-deny locally at those pins. `jq`, which `ci/no-fault-points.sh` needs, is
not pinned: it comes with the GitHub runners and macOS. So do `ruby`, which
`ci/no-secret-in-errors.sh` and `ci/required-checks.sh` need, and, on the runners, `gh`, which the
latter needs.
