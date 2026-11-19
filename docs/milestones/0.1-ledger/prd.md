# 0.1 Ledger: PRD

## Problem

Everything the engine does later (runs, tenancy, doors) writes through one commit path. That path must be durable, ordered and readable before anything is built on it, and it must be provable on its own.

## Who uses it

- **Embedders** that link the foundation crates: the 0.2 operation layer, the 0.3 host, later a mobile app.
- **The behaviors suite**, which drives it in process through `Operations`.

## After 0.1 you can

- Open a ledger for one environment in a directory, and reopen it after a crash with nothing acknowledged lost.
- Emit an event, or a batch of up to 100, with data, metadata, a version and an idempotency key.
- Be refused, with nothing stored, for a bad name, an owned name, a reserved marker, malformed data or an oversized payload.
- Repeat an idempotency key and get the first event back.
- Read an event by ID, and list events newest first, a page at a time, the moment any client's emit returns.

## Modules → features

The modules are in [`architecture.md`](architecture.md#modules).

| Module | Features |
|---|---|
| Ledger | Durable emit, read your writes, crash safety |
| Event rules | Gates, idempotency, sequence, list order |
| `Operations` interface | One interface for every caller: tests now, doors and remote clients later |
| Core types | Opaque IDs and one time type for every module above |

## Out of scope

| Left out | Arrives in |
|---|---|
| Credentials and scopes, the verified emitter, bootstrap | 0.2 |
| The HTTP door, wire formats, the server binary, the remote client, the CLI | 0.3 |
| Runs, triggers, emits from a run, paging back, totals, filters, listing event names | 0.4 |
| More than one environment; keys per environment | 0.9 |
| Event schemas, entity streams, pub/sub, retention and redaction | 0.13–0.17 |
| Power-loss durability | The simulator, in a later release |

## Done when

1. Every 0.1 behavior passes in process against the ledger through `LocalOperations`.
2. Every proof named in [`invariants.md`](invariants.md) exists and passes, the crash test included.
3. `cargo-deny`, `clippy -D warnings` and the tests pass on Linux and macOS.

Then publish at `0.1.0`: `ironflowx-cell`, `ironflowx-events`, `ironflowx-proto`, `ironflowx-core`, and the `ironflowx` facade re-exporting all four. A crate outside the workspace that depends on `ironflowx` and `tokio`, from crates.io, must open a ledger and emit.

0.1 sets no performance target (D15).

## Build order

One PR or more per step; each step is proven before the next starts.

1. Core types and the CI skeleton: clippy, `cargo-deny`, the LED-15 purity checks, Linux and macOS. This PR deletes the resource-name type.
2. Event rules: `decide` and `fold`, unit-tested against the [rules table](architecture.md#event-rules).
3. The `Operations` trait and its messages.
4. The ledger and `LocalOperations`, with the behaviors and the deterministic tests.
5. The crash test.
6. Publish.
