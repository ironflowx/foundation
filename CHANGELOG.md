# Changelog

Each release lists what it adds and changes. All published crates share one version. Foundation crates are 0.x with no semver-stability promise.

## Unreleased

## 0.1.0 - 2026-11-21

The first release: the 0.1 ledger ([#34](https://github.com/ironflowx/foundation/issues/34)). Depend on the `ironflowx` facade; [`examples/embed`](https://github.com/ironflowx/foundation/blob/v0.1.0/examples/embed/src/main.rs) opens a ledger, emits an event and reads it back.

- **ironflowx-core:** validated opaque IDs, the environment reference, stored and event-clock time, the clock trait and the error kinds.
- **ironflowx-events:** the event rules: what an event is and when an emit is refused.
- **ironflowx-proto:** the Operations interface and its request and answer types.
- **ironflowx-cell:** the ledger, one environment's durable commit log (on [segment-log](https://github.com/ironflowx/segment-log)) and read store, and `LocalOperations`. A read sees every write acknowledged before it. A crash test kills the process at every named fault point and checks that every acknowledged write survives ([invariants.md](https://github.com/ironflowx/foundation/blob/v0.1.0/docs/invariants.md)).
- **ironflowx:** every foundation crate, re-exported.
