# CI checks

`.github/workflows/ci.yml` runs these on every PR. The `main` ruleset requires each job but `soak`.
Every check script here, except the snapshot comparison in `public-api.sh`, has a fixture it must
reject. Fixtures can be dedicated files under `fixtures/` or builds configured to trigger the
check. `must-reject.sh` checks that each fixture fails for the expected reason.

`ci/check.sh` runs every job below but `soak` locally, in order, and stops at the first failure: run it
before you push. `git config core.hooksPath .githooks` makes `git push` run it. Over SSH, also
`git config core.sshCommand 'ssh -o ServerAliveInterval=30'`: GitHub closes the push's connection
while the checks run, and git then dies of SIGPIPE with nothing pushed, so the hook refuses an SSH
push without keepalives. The root
`Makefile` names these commands; `make` lists them.

A release is `make release VERSION=X.Y.Z` (`ci/release.sh`): from a clean `main` that matches
`origin/main` with green CI, it publishes every crate without `publish = false`, then tags and
pushes `vX.Y.Z`. The tag starts `.github/workflows/release.yml`, which creates the GitHub release
from that version's `CHANGELOG.md` section, with no assets. A rerun of `make release` skips crates
that are already published. Nothing here builds a binary: the `ironflowx` binary and its release
archives are [`ironflowx/ironflow`](https://github.com/ironflowx/ironflow)'s (D85).

Run `make check` before rehearsing the release. On a clean checkout,
`make release-dry-run VERSION=X.Y.Z` verifies the unpublished packages via
`cargo publish --dry-run`. The proto package is verified with `wire` enabled and with no
`protoc` or `buf` on PATH.

| Job | Enforces | Run locally | When it fails |
|---|---|---|---|
| `fmt`, `clippy`, `clippy-macos`, `test` | Formatting, and comment lines of at most 100 columns in Rust and `ci/*.sh`, URLs exempt (`ci/comment-width.sh`: rustfmt does not wrap comments); lints with warnings denied, tests; `clippy` builds the workspace with neither `protoc` nor `buf` on `PATH` (SRV-14; `ci/no-protoc.sh`); `test` runs the event rules again with `arbitrary_precision`, the client's URL test again without `client-tls` and with both of `rustls`'s providers, and the crash test on Linux and macOS with `CRASH_KILLS=100` | `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `env -u CRASH_SEED CRASH_KILLS=100 cargo test --workspace`; event rules again with `serde_json`'s `arbitrary_precision` (D12): `cargo test -p ironflowx-events --features serde_json/arbitrary_precision`; the client's URL test without `client-tls`: `NO_CLIENT_TLS=1 cargo test -p ironflowx-proto --test client`; with both providers: `cargo test -p ironflowx-proto --test client --features client-tls,rustls/ring` | Fix the code |
| `deny` | Licences and advisories, Linux and macOS graphs (`deny.toml`) | `cargo deny check` | Replace the dependency, or extend `deny.toml` with a reason |
| `purity` | LED-15: pure crates' dependencies and calls; D17: only `ironflowx-cell` calls `SequencedTime::from_event_clock` (and one helper in `ironflowx-events`' `tests/rules.rs`, which stands in for the ledger); D36: no crate but the credentials crate calls `SubmitAs::key` (`ci/no-key-attribution.sh` inside core), and no crate but the engine calls `Grant::submit_as`; TEN-4: no crate but the control plane calls the key digest's storage codec (`ci/no-digest-codec.sh` backs clippy up by name); every crate-wide ban in every `clippy.toml` (`ci/clippy-bans.sh`); SRV-1: `ironflowx-doors` depends on no engine crate (`ci/no-engine-deps.sh`); SRV-2 and SRV-12: a proto the generator refuses fails its build, one package of `ci/fixtures/codegen` per refusal; `ci/red.sh`, which records a test check landing red, and `ci/must-reject.sh`; `ci/app-ref.sh`, which reads the app branch `conformance` tests (D122), one fixture of `ci/fixtures/app-ref` per refusal and one branch it must read; crash test: no published crate (Cargo's `publish` is not `false`, set or inherited) can build with `__fault-points`: by default, through a dependency or a feature of its own, on any target; the one route allowed is a crate's own `__fault-points` forwarding to a dependency's; a dev-dependency, which does not ship, does not count (D23; `ci/no-fault-points.sh`, which needs `jq`) | `ci/purity.sh` | See [Pure crates](#pure-crates); for an engine crate, call it through `Operations`; for the generator, say what the proto leaves out; for `__fault-points`, drop it from the published crate's defaults or dependency |
| `wasm` | A pure crate builds for WASM: `cargo check --target wasm32-unknown-unknown` of `ironflowx-core`, `ironflowx-events` and `ironflowx-authz`, one crate a run, so no other crate's features build it. A dependency or a cfg without wasm support fails it; threads, the clock and the filesystem still compile there, so `purity`'s bans keep them out | `rustup target add wasm32-unknown-unknown`, then `cargo check --locked --target wasm32-unknown-unknown -p <crate>` for each | Replace the dependency or the code that has no wasm support |
| `conformance` | [`ironflowx/ironflow`](https://github.com/ironflowx/ironflow)'s suite, its binary, CLI, behaviors and server crash test included, against this checkout, on Linux and macOS: the job checks out the branch `ci/app-ref` names, as `refs/heads/<branch>` so no tag of that name can stand in, `main` unless a PR changes it (D122; see [A change the app must follow](#a-change-the-app-must-follow)), and runs its `ci/conformance.sh`, which points every foundation crate it uses at this checkout through a `[patch]`. A change here that breaks the app fails before it merges. A PR that changes only `docs/` and Markdown files passes it without running the suite: its steps are skipped, not the job, which would then pass every PR. A malformed `ci/app-ref` fails it, naming the file (`ci/app-ref.sh`; `ci/purity.sh` runs its fixtures, in `ci/fixtures/app-ref`) | `ci/check.sh` fetches the same `refs/heads/<branch>` into a temporary directory and runs `ci/conformance.sh .`; by hand, `<ironflow>/ci/conformance.sh <this checkout>` | Fix the change, or, for a change the app must follow, land it as a pair: see [A change the app must follow](#a-change-the-app-must-follow) |
| `embed` | The PRD's last "Done when": `examples/embed`, outside the workspace and on `ironflowx` and `tokio` only, bootstraps an engine, emits, reads the event back with a `read` key and sees that key's emit refused as `PermissionDenied`, on Linux and macOS. Its `.cargo/config.toml` patches `ironflowx` to this checkout; to build the published release instead, delete that file and `Cargo.lock` there, then `cargo run` | `cd examples/embed && cargo fmt --check && cargo clippy --locked -- -D warnings && cargo run --locked` | Fix the facade or the crate it names; a new dependency of the example also needs `cargo update` there |
| `public-api` | D21: each crate's public API matches `public-api/<crate>.txt`, `ironflowx-proto`'s with `client` and without `wire` and `server`; LED-9: nothing public takes a bare `EnvironmentId`; no public API names a dependency's type, but the `axum::Router` the door returns; `ci/api-rules.sh`: nothing turns an `Address` into an ID (TEN-8), `KeyDigest` exposes nothing but `ct_eq` and its codec (TEN-4); `ci/no-secret-in-errors.sh`, over rustdoc's JSON, private and hidden items included: no error type holds a `Secret`, or is used with one (TEN-3); each `compile_fail` doctest names an error code (`ci/compile-fail-codes.sh`) and fails with it, which only nightly rustdoc checks | `ci/public-api.sh` | Review the printed diff, then `ci/public-api.sh --bless` and commit the snapshot; a new `pub use` line of ours in a snapshot also needs its name in `ci/no-dependency-types.sh` |
| `doc-pins` | D16: docs name only the `segment-log-core` version `crates/ironflowx-cell/Cargo.toml` pins (tag-pinned links) | `ci/doc-pins.sh` | Re-pin the printed links to the new tag, or drop the version from the line |
| `maintainer-only` | No outside contributions: a PR must come from a branch of this repository, which only those with write access, and Dependabot, can push. A push to `main` passes it | None: it reads the pull request | Close the PR; a maintainer opens the change from a branch here |
| `soak` | The crash test's longer soak before publishing: `inputs.kills` seeded kills (10000 by default) on Linux and macOS. Runs only when started by hand, with every other job, and is not required | `gh workflow run ci --ref <branch> -f kills=10000`; on your machine, `env -u CRASH_SEED CRASH_KILLS=10000 cargo test -p ironflowx-crash-test --locked --test crash seeded_random_kills`. The server crash soak, of the binary, is [`ironflowx/ironflow`](https://github.com/ironflowx/ironflow)'s | Replay the printed `CRASH_SEED=<seed> CRASH_KILLS=1` and fix the ledger, the open or the key change it fails in |
| `lint-ci` | actionlint and shellcheck over the workflow, `ci/*.sh` and `.githooks/*`; the `main` ruleset requires exactly the jobs of `ci.yml` but `soak` (`ci/required-checks.sh`, which reads the live ruleset) | `actionlint && shellcheck -x ci/*.sh .githooks/* && ci/required-checks.sh` | Fix the script; for a job, add it to the ruleset's required checks, or remove it there |

The `clippy`, `clippy-macos`, `test`, `wasm`, `conformance`, `embed` and `public-api` jobs
restore a build cache (`Swatinem/rust-cache`). Only a push to `main` saves one, so PRs do not
evict each other from the repository's cache quota. A stale cache only slows a build: Cargo
rebuilds what changed.

The `test` job also builds and tests the facade in three separate Cargo invocations, with its
default features, with `client`, and with `client-tls`. Each asserts exactly which facade features
are on. Both client builds compile the `whoami` example and check the schemes the client accepts.
Separate invocations keep the rest of the workspace from switching the proto client's features on.
`ci/no-facade-http.sh` also checks the default facade dependency graph for the HTTP and TLS
packages used by the proto client. The facade with `client-tls` enabled supplies its rejecting
fixture.

## A change the app must follow

[`ironflowx/ironflow`](https://github.com/ironflowx/ironflow) implements `Operations` itself and
lists the operations in its scenarios, so a foundation PR that adds an operation, a scope or an
error kind fails `conformance` against the app's `main`, and the app cannot change first. Such a
change lands as a pair (D122):

1. The app's change goes on a branch of `ironflowx/ironflow`, pinned to the foundation PR's head
   commit.
2. The foundation PR replaces the one line of `ci/app-ref`, `main`, with that branch's name, so
   `conformance` builds and runs the app at that branch against the PR.
3. The foundation PR merges. The app's branch moves its pin to the merge commit and merges to the
   app's `main`. The app deletes a branch when its PR merges, so restore it at once (the PR's
   "Restore branch"): `main` names it, and its checkout fails while it is gone.
4. The next foundation PR sets `ci/app-ref` back to `main`. The app's branch is kept until it has:
   until then, `main` and every PR that runs the suite test the app at that branch.

A foundation PR that breaks nothing in the app leaves the file alone. The file holds one line, a
branch name, and nothing else: a missing file, an empty one, more than one line, a byte other than
an ASCII letter, a digit, `.`, `_`, `-` or `/` (a trailing space, a CR, a BOM), or a name git
refuses for a branch (`git check-ref-format --branch`) fails `conformance` with a message that names
the file. A name git takes with another byte, `+`, `@` or a non-ASCII letter, is refused too: name
the app's branch without them. A well-formed name of no branch, a typo or `origin/<branch>`, fails
at the checkout, whose message names the ref but not the file (a tag or a branch literally named
`refs/heads/<name>` would stand in for the branch; the app has neither). A commit or a tag cannot be
named.

## Pure crates

`ironflowx-core`, `ironflowx-events` and `ironflowx-authz` are pure (LED-15). Each has:

- `ci/purity/<crate>.allow`: every crate it may depend on, transitively. A new dependency of a
  pure crate goes here, with each crate it pulls in; `ci/check-deps.sh` prints what is missing.
- `clippy.toml`: core's file bans the clock, filesystem, network, threads, OS randomness and
  `HashMap`; events links to it, and authz copies it less the `SubmitAs::key` ban
  (`ci/clippy-bans.sh` checks the copy). A pure crate uses `BTreeMap`.
- The `#![forbid(...)]` header at the top of `src/lib.rs`, so an inner `#[allow]` cannot lift a ban.

A new pure crate needs all three, plus a `ci/check-deps.sh` line in `ci/purity.sh` and its name in the `wasm` job's list, in `ci.yml` and `ci/check.sh`. A new crate of
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
