# Changelog

Each release lists what it adds and changes. All published crates share one version. Foundation crates are 0.x with no semver-stability promise.

## Unreleased

- **README:** states the purpose and shows the 0.1 architecture with the crate and emit-then-read diagrams. Its links and images are absolute, so they work on crates.io; 0.1.0's example link there is broken. The emit-then-read diagram shows the committed head and applied position as `p + 1`, as the code does.
- **Ledger** ([#65](https://github.com/ironflowx/foundation/pull/65)): at most 16 submissions wait for the writer; more callers wait for a slot (D24). `open` takes a new `<dir>/ledger.lock`, so a sealed log, whose own lock is released, still answers `Locked` to a second opener. A read store whose newest event disagrees with the log is rebuilt. With an empty log, a read store whose applied position cannot be read answers `Io` and is kept, and one with recorded progress answers `Corrupt` before genesis is written. `open` syncs the parent of each directory it creates. If the writer dies, a submission queued or being decided answers `Unavailable`, not `Unknown`; only one whose append began answers `Unknown`.
- **Docs:** the C3 and restart diagrams no longer show a pending view or a rollback, and the restart diagram writes the applied position as one past the record. The `Corrupt` runbook adds the case where the ledger refuses a record, and narrows when segment-log's `repair truncate` refuses a class F plan. A sealed log's first append makes reads `Unavailable` too. The emit-read diagram's fold runs after the ack, not asynchronously. `invariants.md`: the public-API check covers every workspace crate, and `__fault-points` is an opt-in no published crate enables, not one no build can enable.

## 0.1.0 - 2026-11-21

The first release: the 0.1 ledger ([#34](https://github.com/ironflowx/foundation/issues/34)). Depend on the `ironflowx` facade; [`examples/embed`](https://github.com/ironflowx/foundation/blob/v0.1.0/examples/embed/src/main.rs) opens a ledger, emits an event and reads it back.

- **ironflowx-core:** validated opaque IDs, the environment reference, stored and event-clock time, the clock trait and the error kinds.
- **ironflowx-events:** the event rules: what an event is and when an emit is refused.
- **ironflowx-proto:** the Operations interface and its request and answer types.
- **ironflowx-cell:** the ledger, one environment's durable commit log (on [segment-log](https://github.com/ironflowx/segment-log)) and read store, and `LocalOperations`. A read sees every write acknowledged before it. A crash test kills the process at every named fault point and checks that every acknowledged write survives ([invariants.md](https://github.com/ironflowx/foundation/blob/v0.1.0/docs/invariants.md)).
- **ironflowx:** every foundation crate, re-exported.
