# 0.1 Ledger: behaviors

What a client observes in 0.1. Invariants that no single example can show are in [`../invariants.md`](../invariants.md); the rules these scenarios sample are in [`../architecture.md`](../architecture.md#event-rules).

## Files

| File | Scenarios | IDs |
|---|---|---|
| [`events.feature`](events.feature) | 41 | Emit EVT-001–009 · gates EVT-010–017, EVT-038–039 · batch EVT-020–025 · idempotency EVT-030–037 · reading EVT-040–055 · read your writes EVT-060–062 |

IDs missing from these ranges are unassigned, except 005, which is retired.

Each scenario is mapped to the spec stories that own it in [Spec 4, "Behavior coverage"](../specs/04-ledger.md#behavior-coverage).

## How they run

The steps call `Operations`, never a URL or a status code. One set of steps, two adapters:

| Adapter | Drives | When |
|---|---|---|
| `LocalOperations` | A ledger in the test's process, on a temporary directory, with a `ManualClock` | Every PR |
| Remote client | The server binary at `IRONFLOWX_URL` | From 0.3 |

The runner is the `cucumber` crate, in an unpublished workspace crate (D20). It runs these files from this folder; they are not copied.

## Conventions

- Tags: a stable `@EVT-nnn` ID and `@release-0.1`. IDs are never reused.
- A quoted table cell is a JSON string literal after Gherkin unescaping. A data cell is the JSON sent, byte for byte.
- A cell that describes its input (`128 nested arrays`, `a string holding the byte 0xFF`) is built by the step: a feature file is UTF-8 and cannot hold an invalid byte itself.
- Lengths count UTF-8 bytes. Sizes are the bytes of the JSON as sent.
- The background is "a fresh environment and a client": no credential in 0.1. "Another client" is a second `LocalOperations` over the same ledger.
- "N days ago" steps the `ManualClock`.
- `@local`: the scenario needs a fixture only `LocalOperations` has (the clock, a second incarnation). The remote adapter skips it until it can drive them.

## Not in 0.1

| Left out | Where |
|---|---|
| The verified emitter | 0.2 |
| The wire format, timestamps on the wire, a limit the wire cannot carry, invalid UTF-8 in an idempotency key | 0.3 (door) |
| Emits from a run, the version a function receives, paging back, totals, filters, listing event names | 0.4 |
| Keys per environment | 0.9 |
| Restart durability as a scenario | A property: the crash test proves it (`../invariants.md`) |
