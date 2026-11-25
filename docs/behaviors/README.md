# Behaviors

What a client observes. Invariants that no single example can show are in [`../invariants.md`](../invariants.md); the rules these scenarios sample are in [`../architecture.md`](../architecture.md).

## Files

| File | Scenarios | IDs |
|---|---|---|
| [`cli.feature`](cli.feature) | 14 | Calling CLI-001–002, CLI-013–014 · keys CLI-006–007 · errors CLI-003–004, CLI-015 · describe CLI-008–010 · seeds, version CLI-012, CLI-016 |
| [`events.feature`](events.feature) | 41 | Emit EVT-001–009 · gates EVT-010–017, EVT-038–039 · batch EVT-020–025 · idempotency EVT-030–037 · reading EVT-040–055 · read your writes EVT-060–062 |
| [`serve.feature`](serve.feature) | 26 | First start and the bootstrap key HOST-001–005, HOST-017–019, HOST-030 · health HOST-006 · starting, stopping, refusing HOST-009–013, HOST-016 · settings HOST-014–015 · the wire HOST-021–027, HOST-029 |
| [`tenancy.feature`](tenancy.feature) | 40 | Bootstrap and open ACC-001–018 · authentication ACC-020–024 · scopes ACC-030–032 · creating and listing keys ACC-040–047 · revoking and expiry ACC-050–054 · who emitted ACC-060–061 |

IDs missing from these ranges are unassigned (CLI-005 too), except EVT-005, HOST-028 and CLI-011, which are retired.

Each scenario of 0.1, in `events.feature`, is mapped to the spec stories that own it in [Spec 4, "Behavior coverage"](https://github.com/ironflowx/foundation/blob/93f0bb261276d7522180d7307464256239a44030/docs/milestones/0.1-ledger/specs/04-ledger.md#behavior-coverage). The [coverage sweep](https://github.com/ironflowx/foundation/blob/a192bdb8f63e08ef85cf4549ffeac8c3f3dc7907/docs/behaviors/coverage.md), a dated planning record of the first release, lists the items deferred to later releases for when they are drafted.

## How they run

The steps call `Operations`, never a URL or a status code, but for `/health` and `/ready`, which are the interface. Those of `serve.feature` also start the binary, read its log and, where a scenario names a protocol, call with a client of that protocol generated from the proto, or, where it is about the JSON itself, read what that protocol sent; those of `cli.feature` start the binary and run commands of it, and read their stdout, stderr and exit code:

| Adapter | Drives | When |
|---|---|---|
| The engine (`Engine`) | An engine in the test's process, on a temporary directory, with a `ManualClock` | Every PR: `events.feature`, `tenancy.feature`, whole |
| Proto client (`Client`) | The `ironflowx` binary, built once at the suite's start with `cargo build -p ironflowx-bin --locked` (Cargo gives a test a binary's path only within its own package), started by each scenario on a fresh data directory and `127.0.0.1:0` | Every PR: `events.feature` and `tenancy.feature` again, but for what is `@local`; `serve.feature`, but for the calls that name a protocol |
| The CLI | The same binary, each command a process of its own with the scenario's variables, calling an engine the scenario serves | Every PR: `cli.feature` |

Against the binary, "a fresh environment" and "a fresh engine directory" are a binary started on a fresh data directory; "the bootstrap key" is the key in `admin.key`; and "now", "in one hour" and "an hour ago" are the machine's clock, read once per scenario, so ACC-010's creation time is checked to lie between that and the answer. "A client" and "another client" call through the scenario's one `Client`. `Client` refuses EVT-016's two rows that are not UTF-8 itself, and HOST-025 sends such bytes raw.

The runner is the `cucumber` crate, in an unpublished workspace crate (D20). It runs the files in this folder in place; they are not copied. It fails unless the `.feature` files here are exactly those its `SCENARIOS` table names, each with that many scenario declarations (before outline expansion), so a new scenario updates its count and a new file is added there (`crates/ironflowx-behaviors/tests/behaviors.rs`). Each file runs on its own World: an engine each, or a started binary. Every scenario of `events.feature` and `tenancy.feature` runs on the engine, and every one that is not `@local` against the binary; of `serve.feature` and `cli.feature`, those in each `REGISTERED` (`tests/behaviors/serve.rs`, `tests/behaviors/cli.rs`): each lands with the work that makes it pass. The runner fails unless as many scenarios of each file pass with each adapter, an outline once per example, as `EVENT_RUNS` and `EVENT_REMOTE_RUNS` (95 and 93), `RUNS` and `REMOTE_RUNS` of `tests/behaviors/tenancy.rs` (70 and 41) and each `RUNS` of `serve.rs` and `cli.rs` say. Run them alone with `cargo test -p ironflowx-behaviors --test behaviors`.

## Conventions

- Tags: a stable `@EVT-nnn`, `@ACC-nnn`, `@HOST-nnn` or `@CLI-nnn` ID and the release that added it, as `@release-0.1`; `@door:cli` on every scenario of `cli.feature`. IDs are never reused.
- A quoted table cell is a JSON string literal after Gherkin unescaping. A data cell is the JSON sent, byte for byte.
- A data or metadata cell that starts `built:` describes its input (`built: 128 nested arrays`, `built: a string holding the byte 0xFF`), and the step builds it: a feature file is UTF-8 and cannot hold an invalid byte itself. A description the step does not know fails the step; it is never sent as data.
- Lengths count UTF-8 bytes. Sizes are the bytes of the JSON as sent.
- In `events.feature`, "a fresh environment" is `Engine::open` on a temporary directory, "a client" is a call holding the bootstrap key, and "another client" is a call on the same engine holding a key the bootstrap key created with all three scopes. EVT-055's other incarnation is a bare `Ledger` with the same ID and a new token, written as an embedder (D36).
- "N days ago", "N hours pass" and "the clock reads" step the `ManualClock`.
- `@local`: the scenario needs a fixture only the in-process adapter has (the clock, a second incarnation, the data directory, what `open` returns), so it does not run against the binary.
- In `tenancy.feature`, "opened", "closed" and "reopened" are `Engine::open` and `close` on the scenario's directory; "the bootstrap key" is the key the first open returned, or the seed it was given; a seed is a well-formed key the step builds; "a client with a `<scope>` key" holds a key the bootstrap key created with exactly those scopes.
- In `serve.feature`, "served" is `ironflowx serve` on the scenario's data directory, on `127.0.0.1:0` unless an address is named; "becomes ready" is `/ready` answering 200. "Over Connect", "over gRPC" and so on use a client for that protocol generated from the proto, not `Client`. A data cell is the JSON itself, unquoted, or `built:` and a description.
- In `cli.feature`, an envelope is the one line of stderr, with nothing on stdout; "exits with code N" is the process's status; single quotes in a command are the shell's, and `sh` runs it, in the scenario's directory; a key with scopes is one the CLI creates with the bootstrap key; a printed cell is the JSON itself, unquoted.

## Not yet

| Left out | Where |
|---|---|
| The scenarios of `serve.feature` and `cli.feature` that are not in `REGISTERED` | Later in 0.3 |
| Emits from a run, the version a function receives, paging back, totals, filters, listing event names | 0.4 |
| Approval requests | 0.7 |
| Users, roles, policies, audit, a key's last use, more than one environment | 0.9 |
| A key in a live-stream handshake | 0.15 |
| Restart durability as a scenario | A property: the crash test proves it (`../invariants.md`) |
