# Behaviors

What a client observes. Invariants that no single example can show are in [`../invariants.md`](../invariants.md); the rules these scenarios sample are in [`../architecture.md`](../architecture.md).

## Files

| File | Scenarios | IDs |
|---|---|---|
| [`events.feature`](events.feature) | 41 | Emit EVT-001–009 · gates EVT-010–017, EVT-038–039 · batch EVT-020–025 · idempotency EVT-030–037 · reading EVT-040–055 · read your writes EVT-060–062 |
| [`serve.feature`](serve.feature) | 26 | First start and the bootstrap key HOST-001–005, HOST-017–019, HOST-030 · health HOST-006 · starting, stopping, refusing HOST-009–013, HOST-016 · settings HOST-014–015 · the wire HOST-021–027, HOST-029 |
| [`tenancy.feature`](tenancy.feature) | 40 | Bootstrap and open ACC-001–018 · authentication ACC-020–024 · scopes ACC-030–032 · creating and listing keys ACC-040–047 · revoking and expiry ACC-050–054 · who emitted ACC-060–061 |

IDs missing from these ranges are unassigned, except EVT-005 and HOST-028, which are retired.

Each scenario of 0.1, in `events.feature`, is mapped to the spec stories that own it in [Spec 4, "Behavior coverage"](https://github.com/ironflowx/foundation/blob/93f0bb261276d7522180d7307464256239a44030/docs/milestones/0.1-ledger/specs/04-ledger.md#behavior-coverage). The [coverage sweep](https://github.com/ironflowx/foundation/blob/a192bdb8f63e08ef85cf4549ffeac8c3f3dc7907/docs/behaviors/coverage.md), a dated planning record of the first release, lists the items deferred to later releases for when they are drafted.

## How they run

The steps call `Operations`, never a URL or a status code, but for `/health` and `/ready`, which are the interface. Those of `serve.feature` also start the binary and read its log:

| Adapter | Drives | When |
|---|---|---|
| The engine (`Engine`) | An engine in the test's process, on a temporary directory, with a `ManualClock` | Every PR: `events.feature`, `tenancy.feature` |
| Proto client (`Client`) | The `ironflowx` binary, built once at the suite's start with `cargo build -p ironflowx-bin --locked` (Cargo gives a test a binary's path only within its own package), started by each scenario on a fresh data directory and `127.0.0.1:0` | Every PR: `serve.feature` |

The runner is the `cucumber` crate, in an unpublished workspace crate (D20). It runs the files in this folder in place; they are not copied. It fails unless the `.feature` files here are exactly those its `SCENARIOS` table names, each with that many scenario declarations (before outline expansion), so a new scenario updates its count and a new file is added there (`crates/ironflowx-behaviors/tests/behaviors.rs`). Each file runs on its own World: an engine each, or a started binary. Every scenario of `events.feature` and `tenancy.feature` runs; of `serve.feature`, those in `REGISTERED` (`tests/behaviors/serve.rs`): each lands with the work that makes it pass. The runner fails unless as many scenarios of each file pass, an outline once per example, as `EVENT_RUNS` and each `RUNS` say (`tests/behaviors/tenancy.rs`, `tests/behaviors/serve.rs`). Run them alone with `cargo test -p ironflowx-behaviors --test behaviors`.

## Conventions

- Tags: a stable `@EVT-nnn`, `@ACC-nnn` or `@HOST-nnn` ID and the release that added it, as `@release-0.1`. IDs are never reused.
- A quoted table cell is a JSON string literal after Gherkin unescaping. A data cell is the JSON sent, byte for byte.
- A data or metadata cell that starts `built:` describes its input (`built: 128 nested arrays`, `built: a string holding the byte 0xFF`), and the step builds it: a feature file is UTF-8 and cannot hold an invalid byte itself. A description the step does not know fails the step; it is never sent as data.
- Lengths count UTF-8 bytes. Sizes are the bytes of the JSON as sent.
- In `events.feature`, "a fresh environment" is `Engine::open` on a temporary directory, "a client" is a call holding the bootstrap key, and "another client" is a call on the same engine holding a key the bootstrap key created with all three scopes. EVT-055's other incarnation is a bare `Ledger` with the same ID and a new token, written as an embedder (D36).
- "N days ago", "N hours pass" and "the clock reads" step the `ManualClock`.
- `@local`: the scenario needs a fixture only the in-process adapter has (the clock, a second incarnation, the data directory, what `open` returns). The proto client skips it until it can drive them.
- In `tenancy.feature`, "opened", "closed" and "reopened" are `Engine::open` and `close` on the scenario's directory; "the bootstrap key" is the key the first open returned, or the seed it was given; a seed is a well-formed key the step builds; "a client with a `<scope>` key" holds a key the bootstrap key created with exactly those scopes.
- In `serve.feature`, "served" is `ironflowx serve` on the scenario's data directory, on `127.0.0.1:0` unless an address is named; "becomes ready" is `/ready` answering 200. "Over Connect", "over gRPC" and so on use a client for that protocol generated from the proto, not `Client`.

## Not yet

| Left out | Where |
|---|---|
| The scenarios of `serve.feature` that are not in `REGISTERED`; `events.feature` and `tenancy.feature` through the proto client | Later in 0.3 |
| Timestamps on the wire, a limit the wire cannot carry, invalid UTF-8 in an idempotency key | Later in 0.3 |
| Emits from a run, the version a function receives, paging back, totals, filters, listing event names | 0.4 |
| The CLI | Later in 0.3 |
| Approval requests | 0.7 |
| Users, roles, policies, audit, a key's last use, more than one environment | 0.9 |
| A key in a live-stream handshake | 0.15 |
| Restart durability as a scenario | A property: the crash test proves it (`../invariants.md`) |
