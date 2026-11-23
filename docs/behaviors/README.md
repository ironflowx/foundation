# Behaviors

What a client observes. Invariants that no single example can show are in [`../invariants.md`](../invariants.md); the rules these scenarios sample are in [`../architecture.md`](../architecture.md).

## Files

| File | Scenarios | IDs |
|---|---|---|
| [`events.feature`](events.feature) | 41 | Emit EVT-001–009 · gates EVT-010–017, EVT-038–039 · batch EVT-020–025 · idempotency EVT-030–037 · reading EVT-040–055 · read your writes EVT-060–062 |
| [`tenancy.feature`](tenancy.feature) | 40 | Bootstrap and open ACC-001–018 · authentication ACC-020–024 · scopes ACC-030–032 · creating and listing keys ACC-040–047 · revoking and expiry ACC-050–054 · who emitted ACC-060–061 |

IDs missing from these ranges are unassigned, except EVT-005, which is retired.

Each scenario is mapped to the spec stories that own it in [Spec 4, "Behavior coverage"](https://github.com/ironflowx/foundation/blob/93f0bb261276d7522180d7307464256239a44030/docs/milestones/0.1-ledger/specs/04-ledger.md#behavior-coverage). The [coverage sweep](https://github.com/ironflowx/foundation/blob/a192bdb8f63e08ef85cf4549ffeac8c3f3dc7907/docs/behaviors/coverage.md), a dated planning record of the first release, lists the items deferred to later releases for when they are drafted.

## How they run

The steps call `Operations`, never a URL or a status code. One set of steps, whichever the adapter:

| Adapter | Drives | When |
|---|---|---|
| The engine (`Engine`) | An engine in the test's process, on a temporary directory, with a `ManualClock` | Every PR |
| Remote client | The server binary at `IRONFLOWX_URL` | From 0.3 |

The runner is the `cucumber` crate, in an unpublished workspace crate (D20). It runs the files in this folder in place; they are not copied. It fails unless the `.feature` files here are exactly those its `SCENARIOS` table names, each with that many scenario declarations (before outline expansion), so a new scenario updates its count and a new file is added there (`crates/ironflowx-behaviors/tests/behaviors.rs`). Each file runs on its own World, an engine each. Only the tenancy scenarios in `REGISTERED` (`tests/behaviors/tenancy.rs`) run: each lands with the work that makes it pass, with how many times it runs (an outline once per example), and the runner fails unless that many registered runs pass. Run them alone with `cargo test -p ironflowx-behaviors --test behaviors`.

## Conventions

- Tags: a stable `@EVT-nnn` or `@ACC-nnn` ID and the release that added it, `@release-0.1` or `@release-0.2`. IDs are never reused.
- A quoted table cell is a JSON string literal after Gherkin unescaping. A data cell is the JSON sent, byte for byte.
- A data or metadata cell that starts `built:` describes its input (`built: 128 nested arrays`, `built: a string holding the byte 0xFF`), and the step builds it: a feature file is UTF-8 and cannot hold an invalid byte itself. A description the step does not know fails the step; it is never sent as data.
- Lengths count UTF-8 bytes. Sizes are the bytes of the JSON as sent.
- In `events.feature`, "a fresh environment" is `Engine::open` on a temporary directory, "a client" is a call holding the bootstrap key, and "another client" is a call on the same engine holding a key the bootstrap key created with all three scopes. EVT-055's other incarnation is a bare `Ledger` with the same ID and a new token, written as an embedder (D36).
- "N days ago" steps the `ManualClock`.
- `@local`: the scenario needs a fixture only the in-process adapter has (the clock, a second incarnation, the data directory, what `open` returns). The remote adapter skips it until it can drive them.
- In `tenancy.feature`, "opened", "closed" and "reopened" are `Engine::open` and `close` on the scenario's directory; "the bootstrap key" is the key the first open returned; a seed is a well-formed key the step builds.

## Not yet

| Left out | Where |
|---|---|
| The wire format, timestamps on the wire, a limit the wire cannot carry, invalid UTF-8 in an idempotency key | 0.3 (door) |
| Emits from a run, the version a function receives, paging back, totals, filters, listing event names | 0.4 |
| More than one environment | 0.9 |
| Restart durability as a scenario | A property: the crash test proves it (`../invariants.md`) |
