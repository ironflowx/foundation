# Behaviors

What a client observes. Invariants that no single example can show are in [`../invariants.md`](../invariants.md); the rules these scenarios sample are in [`../architecture.md`](../architecture.md).

## Files

| File | Scenarios | IDs |
|---|---|---|
| [`events.feature`](events.feature) | 41 | Emit EVT-001–009 · gates EVT-010–017, EVT-038–039 · batch EVT-020–025 · idempotency EVT-030–037 · reading EVT-040–055 · read your writes EVT-060–062 |
| [`tenancy.feature`](tenancy.feature) | 40 | Bootstrap and open ACC-001–018 · authentication ACC-020–024 · scopes ACC-030–032 · creating and listing keys ACC-040–047 · revoking and expiry ACC-050–054 · who emitted ACC-060–061 |
| [`functions.feature`](functions.feature) | 19 | Definitions FUN-001–008 · triggers FUN-010–014 · starting a run FUN-020–025 |
| [`execution.feature`](execution.feature) | 50 | Claiming EXE-001–008 · steps and replay EXE-010–019 · failure and retries EXE-020–028 · leases and the fence EXE-030–039 · the end of a run EXE-040–047 · reading runs EXE-050–051 · restarts and clocks EXE-052–054 |

IDs missing from these ranges are unassigned, except EVT-005, which is retired. `serve.feature` and `cli.feature`, the `HOST-nnn` and `CLI-nnn` scenarios of the binary and its CLI, are in [`ironflowx/ironflow`](https://github.com/ironflowx/ironflow) (D85), which also runs these two files against the binary. Foundation's `conformance` job runs that suite against each foundation PR that is not only `docs/` and Markdown, at the app branch `ci/app-ref` names (D122).

Each scenario of 0.1, in `events.feature`, is mapped to the spec stories that own it in [Spec 4, "Behavior coverage"](https://github.com/ironflowx/foundation/blob/93f0bb261276d7522180d7307464256239a44030/docs/milestones/0.1-ledger/specs/04-ledger.md#behavior-coverage). The [coverage sweep](https://github.com/ironflowx/foundation/blob/a192bdb8f63e08ef85cf4549ffeac8c3f3dc7907/docs/behaviors/coverage.md), a dated planning record of the first release, lists the items deferred to later releases for when they are drafted.

## How they run

The steps call `Operations`, never a URL or a status code:

| Adapter | Drives | When |
|---|---|---|
| The engine (`Engine`) | An engine in the test's process, on a temporary directory, with a `ManualClock` | Every PR: `events.feature`, `tenancy.feature`, whole |
| Proto client (`Client`) | An `ironflowx-doors` router over an engine in the test's process, opened by each scenario on a fresh directory and served on `127.0.0.1:0` | Every PR: every file again, but for what is `@local` |

Through the door, "a fresh environment" and "a fresh engine directory" are an engine opened on a fresh directory with the machine's clock, as a host opens it (in `functions.feature` and `execution.feature`, with the scenario's `ManualClock`, so a step can move time under either adapter); "the bootstrap key" is the key that open minted; and "now", "in one hour" and "an hour ago" are the machine's clock, read once per scenario, so ACC-010's creation time is checked to lie between that and the answer. "A client" and "another client" call through the scenario's one `Client`. `Client` refuses EVT-016's two rows that are not UTF-8 itself.

The runner is the `cucumber` crate, in an unpublished workspace crate (D20). It runs the files in this folder in place; they are not copied. It fails unless the `.feature` files here are exactly those its `SCENARIOS` table names, each with that many scenario declarations (before outline expansion), so a new scenario updates its count and a new file is added there (`crates/ironflowx-behaviors/tests/behaviors.rs`). Each file runs on its own World: an engine each, or an engine and its door. Every scenario runs on the engine. All scenarios without `@local` run again through the door. The runner checks the number of passing cases, counting each outline example separately. Run the scenarios with `cargo test -p ironflowx-behaviors --locked --test behaviors`.

**Pending.** A 0.4 scenario, or an `Examples` block of one, tagged `@pending` is not built yet. The runner runs the pending cases of a file apart, without failing them on a step it has no definition for or one that fails, prints how many there are, and checks that number; a pending case that passes fails the run, so its tag is dropped once it can be. Built so far: FUN-001 to FUN-007, FUN-008's rows for the operations that exist, EXE-001, EXE-003 to EXE-008, EXE-031 and EXE-039. EXE-006 and EXE-007 wait in real time under either adapter: EXE-006 starts its run once the engine's clock has told of the claim's admission and read, through either adapter, so the claim has found nothing and waits. FUN-003's last step, "charge" has no run, is a claim of both labels that answers empty, and cannot fail until events create runs. EXE-010's replay needs a run claimed twice, which `ReleaseRun` makes possible; until it lands, the runner checks the same replay with one execution, on the engine and through the door, before the scenarios. No 0.4 scenario may stay pending at the release.

## Conventions

- Tags: a stable `@EVT-nnn` or `@ACC-nnn` ID and the release that added it, as `@release-0.1`. IDs are never reused.
- A quoted table cell is a JSON string literal after Gherkin unescaping. A data cell is the JSON sent, byte for byte.
- A data or metadata cell that starts `built:` describes its input (`built: 128 nested arrays`, `built: a string holding the byte 0xFF`), and the step builds it: a feature file is UTF-8 and cannot hold an invalid byte itself. A description the step does not know fails the step; it is never sent as data.
- Lengths count UTF-8 bytes. Sizes are the bytes of the JSON as sent. The size limits the scenarios name are the constants of `ironflowx_core::limits` (D120). `check_emit`, which answers as event rules answer an admitted emit, without a submit, is proven by a test in `ironflowx-operations` over each event rule's refusal (LED-3), not by a scenario.
- In `events.feature`, "a fresh environment" is `Engine::open` on a temporary directory, "a client" is a call holding the bootstrap key, and "another client" is a call on the same engine holding a key the bootstrap key created with all four scopes. EVT-055's other incarnation is a bare `Ledger` with the same ID and a new token, written as an embedder (D36).
- EVT-013's owned names include the engine's own events, `ironflowx.run.completed` and `ironflowx.run.failed` (D118), which only the engine appends. A test in `ironflowx-events` shows each refused as EVT-013's names are (RUN-14); EVT-013's examples stay as they are, since `ironflowx/ironflow` runs this file as vendored.
- "N days ago", "N hours pass" and "the clock reads" step the `ManualClock`.
- In `functions.feature` and `execution.feature`, "a fresh engine" is `Engine::open` on a temporary directory with a `ManualClock`, under either adapter, the door's included, and "the builder puts the same definition again" first steps that clock a second; "a builder" holds `read` and `write`, made by the step that names it or, in `execution.feature`, when the function is put; "an executor" holds `execute` alone, and "an executor with another key" a second such key. "Claims" does not wait. A run's input is JSON `null` until `StartRun` takes one.
- `@local`: the scenario needs a fixture only the engine adapter has (the clock, but in `functions.feature` and `execution.feature`; a second incarnation, the data directory, what `open` returns), so it does not run through the door.
- In `tenancy.feature`, "opened", "closed" and "reopened" are `Engine::open` and `close` on the scenario's directory; "the bootstrap key" is the key the first open returned, or the seed it was given; a seed is a well-formed key the step builds; "a client with a `<scope>` key" holds a key the bootstrap key created with exactly those scopes.

## Not yet

| Left out | Where |
|---|---|
| Emits from a run, the version a function receives, paging back, totals, filters, listing event names | 0.4 |
| Approval requests | 0.7 |
| Users, roles, policies, audit, a key's last use, more than one environment | 0.9 |
| A key in a live-stream handshake | 0.15 |
| Restart durability as a scenario | A property: the crash test proves it (`../invariants.md`) |
| The embedder's call as a scenario | In process only, and no door carries it, so the proto client adapter cannot run it: a test per operation in `ironflowx-operations`' `tests/embedder.rs` proves it (TEN-1, TEN-6, D121) |
| What a restart does with its read store: keeps one that matches its log, rebuilds one that diverged from it whatever its last record holds, and refuses one ahead of it or a log in another format | LED-8 and LED-12: the deterministic tests and the crash test (`../invariants.md`) |
