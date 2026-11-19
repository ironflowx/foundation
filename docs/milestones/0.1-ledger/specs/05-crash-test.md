# Spec 5: The crash test

Build-order step 5 of [0.1 Ledger](../README.md). Needs Spec 4. Publishing follows it.

## Problem Statement

The ledger promises that nothing acknowledged is lost, that a batch commits whole or not at all, and that replay never duplicates or reorders. A behavior scenario shows one example of each. None of them shows the promise holding when the process dies at the worst moment, and that is exactly when it matters.

## Solution

A test that kills the ledger's process on purpose. A parent process drives a child that owns the ledger, kills it at named points and then at seeded random ones, reopens the directory and checks every promise. A failing run prints its seed and replays exactly.

## User Stories

1. As a maintainer, I want the process killed before the append, so that I know an unacknowledged emit leaves nothing or a whole record.
2. As a maintainer, I want it killed after fsync and before the acknowledgment, so that I know a durable but unanswered emit is recovered whole.
3. As a maintainer, I want it killed after the acknowledgment and before the fold, so that I know an acknowledged event reaches the read store on reopen.
4. As a maintainer, I want it killed in the middle of a fold, so that I know a record's rows and the applied position move together.
5. As a maintainer, I want it killed during open, so that I know recovery itself can be interrupted.
6. As a maintainer, I want each named kill point to run first and deterministically, so that the known hard cases never depend on luck.
7. As a maintainer, I want seeded random kills after them, so that cases nobody named are also tried.
8. As a maintainer, I want a failing run to print its seed and replay from it, so that a failure can be debugged.
9. As a client, I want every acknowledged event ID to read back after a kill, so that an acknowledgment is a promise.
10. As a client, I want every acknowledged idempotency key to return its original event ID after a kill, so that retries stay safe.
11. As a client, I want the new events of an unanswered request to be all present or all absent, so that a batch is never half committed.
12. As a client, I want the duplicates and refusals in an unanswered request to store nothing, so that a kill cannot turn a refusal into an event.
13. As a client, I want event sequences with no gap and no repeat after any kill, so that commit order stays total.
14. As a client, I want event times that never decrease, even while the clock is stepped both ways, so that event time survives restarts.
15. As a client, I want a cursor recorded before the kill to page from the same place after it, so that paging survives restarts.
16. As a maintainer, I want two reopens after each kill to give identical events, keys, sequences and applied position, so that recovery is repeatable.
17. As a maintainer, I want the test to run on Linux and macOS on every PR, so that both claimed platforms are proven.
18. As a publisher, I want CI to fail if a published build could enable the fault points, so that test hooks never ship switched on.

## Implementation Decisions

- **Shape:** a parent process and a child process talking over a pipe. The child owns the ledger; the parent sends keyed and unkeyed emits and mixed batches, and decides when to kill.
- **Fault points:** the child is built with the `fault-points` cargo feature. Named points: before the append, after fsync and before the acknowledgment, after the acknowledgment and before the fold, mid-fold, during open. At a named point the child and parent meet at a barrier, so the kill lands exactly there.
- **Clock:** the child uses the manual clock, and the seed steps it forwards and backwards.
- **What the parent records:** each acknowledged event ID and key, the cursor of its last list, and each request that got no answer. Every new event carries a unique marker in its metadata, so the parent can find it without its ID. The parent also knows each item's expected outcome.
- **Observation:** the parent reads the read store directly, read-only, for event sequences and the applied position. Everything else it checks through the reopened ledger.
- **After each kill:** reopen twice and assert the properties in the invariants doc's "Crash test" section.
- **Budget:** every PR runs all named kill points and 100 seeded kills, on Linux and macOS. There is no longer scheduled run in 0.1; a longer soak is run by hand before publishing.
- **CI check:** no published crate enables `fault-points` by default or through a dependency.
- **Claim:** a kill keeps the operating system's page cache, so this proves crash durability, not power-loss durability.

## Testing Decisions

- This spec is a test. What makes it a good one: it asserts only what a client or a reopened ledger can observe, it knows the expected outcome of every request it sent, and every failure replays from a printed seed.
- **Seam:** the process boundary. The parent treats the child as a black box behind the pipe, plus a read-only look at the read store for sequences and applied position, which no public interface exposes.
- **Prior art:** the fault hooks added in Spec 4; the segment log's own crash-testing strategy is the nearest model.

## Out of Scope

- Power-loss durability and a seeded fault simulator: a later release.
- Disk-full, I/O error and corruption injection beyond what Spec 4's deterministic tests cover.
- Performance measurement.
- Windows.

## Further Notes

- Invariants proven here, in their fault form: LED-2, LED-11, LED-12, LED-14. See [`../invariants.md`](../invariants.md#crash-test).
- **Publishing (step 6)** is a human step once this passes: bump the workspace to 0.1.0, publish the four crates and the facade, then check that a crate outside the workspace, depending only on the facade and `tokio` from crates.io, can open a ledger and emit.
