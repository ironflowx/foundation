# CI checks

`.github/workflows/ci.yml` runs these on every PR; the `main` ruleset requires each job. Every
check is proven by a fixture in `fixtures/` that it must reject (`must-reject.sh`), so a check
that silently stops working fails CI.

`ci/check.sh` runs every job below locally, in order, and stops at the first failure: run it
before you push. `git config core.hooksPath .githooks` makes `git push` run it.

| Job | Enforces | Run locally | When it fails |
|---|---|---|---|
| `fmt`, `clippy`, `clippy-macos`, `test` | Formatting, lints with warnings denied, tests | `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` | Fix the code |
| `deny` | Licences and advisories, Linux and macOS graphs (`deny.toml`) | `cargo deny check` | Replace the dependency, or extend `deny.toml` with a reason |
| `purity` | LED-15: pure crates' dependencies and calls; D17: only `ironflowx-cell` calls `SequencedTime::from_event_clock` (and one helper in `ironflowx-events`' `tests/rules.rs`, which stands in for the ledger); crash test: no published crate (no `publish = false`) can build with `__fault-points`: by default, through a dependency or a feature of its own, on any target (`ci/no-fault-points.sh`) | `ci/purity.sh` | See [Pure crates](#pure-crates); for `__fault-points`, drop it from the published crate's defaults or dependency |
| `public-api` | D21: each crate's public API matches `public-api/<crate>.txt`; LED-9: nothing public takes a bare `EnvironmentId`; no public API names a dependency's type (`rusqlite`, `segment_log_core`, `tokio`, `uuid`, `getrandom`) | `ci/public-api.sh` | Review the printed diff, then `ci/public-api.sh --bless` and commit the snapshot |
| `doc-pins` | D16: docs name only the `segment-log-core` version `crates/ironflowx-cell/Cargo.toml` pins (tag-pinned links) | `ci/doc-pins.sh` | Re-pin the printed links to the new tag, or drop the version from the line |
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
