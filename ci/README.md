# CI checks

`.github/workflows/ci.yml` runs these on every PR; the `main` ruleset requires each job. Every
check is proven by a fixture in `fixtures/` that it must reject (`must-reject.sh`), so a check
that silently stops working fails CI.

| Job | Enforces | Run locally | When it fails |
|---|---|---|---|
| `fmt`, `clippy`, `clippy-macos`, `test` | Formatting, lints with warnings denied, tests | `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` | Fix the code |
| `deny` | Licences and advisories, Linux and macOS graphs (`deny.toml`) | `cargo deny check` | Replace the dependency, or extend `deny.toml` with a reason |
| `purity` | LED-15: pure crates' dependencies and calls; D17: only `ironflowx-cell` calls `SequencedTime::from_event_clock` (and one helper in `ironflowx-events`' `tests/rules.rs`, which stands in for the ledger) | `ci/purity.sh` | See [Pure crates](#pure-crates) |
| `public-api` | D21: each crate's public API matches `public-api/<crate>.txt`; LED-9: nothing public takes a bare `EnvironmentId` | `ci/public-api.sh` | Review the printed diff, then `ci/public-api.sh --bless` and commit the snapshot |
| `lint-ci` | actionlint and shellcheck over the workflow and `ci/*.sh` | `actionlint && shellcheck -x ci/*.sh` | Fix the script |

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
