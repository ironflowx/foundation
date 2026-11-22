# CI checks

`.github/workflows/ci.yml` runs these on every PR; the `main` ruleset requires each job. Every
check is proven by a fixture in `fixtures/` that it must reject (`must-reject.sh`), so a check
that silently stops working fails CI.

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
| `fmt`, `clippy`, `clippy-macos`, `test` | Formatting, lints with warnings denied, tests; `test` runs the event rules again with `arbitrary_precision`, and the crash test on Linux and macOS with `CRASH_KILLS=100` | `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `env -u CRASH_SEED CRASH_KILLS=100 cargo test --workspace`; event rules again with `serde_json`'s `arbitrary_precision` (D12): `cargo test -p ironflowx-events --features serde_json/arbitrary_precision` | Fix the code |
| `deny` | Licences and advisories, Linux and macOS graphs (`deny.toml`) | `cargo deny check` | Replace the dependency, or extend `deny.toml` with a reason |
| `purity` | LED-15: pure crates' dependencies and calls; D17: only `ironflowx-cell` calls `SequencedTime::from_event_clock` (and one helper in `ironflowx-events`' `tests/rules.rs`, which stands in for the ledger); crash test: no published crate (no `publish = false`) can build with `__fault-points`: by default, through a dependency or a feature of its own, on any target; the one route allowed is a crate's own `__fault-points` forwarding to a dependency's (D23; `ci/no-fault-points.sh`, which needs `jq`) | `ci/purity.sh` | See [Pure crates](#pure-crates); for `__fault-points`, drop it from the published crate's defaults or dependency |
| `embed` | The PRD's last "Done when": `examples/embed`, outside the workspace and on `ironflowx` and `tokio` only, opens a ledger, emits and reads the event back, on Linux and macOS. Its `.cargo/config.toml` patches `ironflowx` to this checkout; to build the published release instead, delete that file and `Cargo.lock` there, then `cargo run` | `cd examples/embed && cargo fmt --check && cargo clippy --locked -- -D warnings && cargo run --locked` | Fix the facade or the crate it names; a new dependency of the example also needs `cargo update` there |
| `public-api` | D21: each crate's public API matches `public-api/<crate>.txt`; LED-9: nothing public takes a bare `EnvironmentId`; no public API names a dependency's type (`rusqlite`, `segment_log_core`, `tokio`, `uuid`, `getrandom`) | `ci/public-api.sh` | Review the printed diff, then `ci/public-api.sh --bless` and commit the snapshot |
| `doc-pins` | D16: docs name only the `segment-log-core` version `crates/ironflowx-cell/Cargo.toml` pins (tag-pinned links) | `ci/doc-pins.sh` | Re-pin the printed links to the new tag, or drop the version from the line |
| `soak` | The crash test's longer soak before publishing: `inputs.kills` seeded kills (10000 by default) on Linux and macOS. Runs only when started by hand, with every other job, and is not required | `gh workflow run ci --ref <branch> -f kills=10000`; on your machine, `env -u CRASH_SEED CRASH_KILLS=10000 cargo test -p ironflowx-crash-test --locked --test crash seeded_random_kills` | Replay the printed `CRASH_SEED=<seed> CRASH_KILLS=1` and fix the ledger |
| `lint-ci` | actionlint and shellcheck over the workflow, `ci/*.sh` and `.githooks/*` | `actionlint && shellcheck -x ci/*.sh .githooks/*` | Fix the script |

## Pure crates

`ironflowx-core` and `ironflowx-events` are pure (LED-15). Each has:

- `ci/purity/<crate>.allow`: every crate it may depend on, transitively. A new dependency of a
  pure crate goes here, with each crate it pulls in; `ci/check-deps.sh` prints what is missing.
- `clippy.toml`: core's file bans the clock, filesystem, network, threads, OS randomness and
  `HashMap`; events links to it. A pure crate uses `BTreeMap`.
- The `#![forbid(...)]` header at the top of `src/lib.rs`, so an inner `#[allow]` cannot lift a ban.

A new pure crate needs all three, plus a `ci/check-deps.sh` line in `ci/purity.sh`. A new crate of
any kind needs its name in the loop in `ci/public-api.sh` and a blessed snapshot.

## Tools

`tools.env` pins every tool version; CI reads it, and `ci/install-tools.sh` installs the nightly,
cargo-public-api and cargo-deny locally at those pins.
