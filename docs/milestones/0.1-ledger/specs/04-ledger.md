# Spec 4: The ledger and `LocalOperations`

Build-order step 4 of [0.1 Ledger](../README.md). Needs Specs 1, 2 and 3.

## Problem Statement

An embedder has rules for what an event is, and an interface to call, but nowhere to keep events. It needs to open one environment in a directory, be told an emit succeeded only once the event is durable, read that event back at once from any client, and reopen the directory after a restart without losing or duplicating anything.

## Solution

A ledger: one environment's commit log, read store and the single writer between them, linked into the embedder's process. The embedder opens it in a directory with an environment reference and a clock. A submission is decided by event rules, appended to the log as one record and fsynced before it is acknowledged; a read waits until the read store has caught up with every write acknowledged before it arrived. `LocalOperations` puts the `Operations` interface over one ledger, and the behavior scenarios run through it.

## User Stories

1. As an embedder, I want to mint a fresh environment reference, so that I can create an environment.
2. As an embedder, I want to open a ledger in an empty or missing directory, so that creating an environment is one call.
3. As an embedder, I want to reopen a ledger with the same environment reference and find every acknowledged event, so that a restart loses nothing.
4. As an embedder, I want an open with another environment ID or incarnation token refused, so that a recreated environment never reads its predecessor's events.
5. As an embedder, I want an open of a directory in another format refused loudly, so that a format change never causes a silent misread.
6. As an embedder, I want a second open of a directory in use refused as locked, so that two writers never share a ledger.
7. As an embedder, I want a corrupt log or a foreign record refused at open, so that damage is never folded into the read store.
8. As an embedder, I want a missing, unreadable or inconsistent read store rebuilt from the log on open, so that the read store is never trusted over the log.
9. As an embedder, I want to close a ledger and have in-flight submissions and the fold finish first, so that shutdown is clean.
10. As an embedder, I want dropping a ledger without closing it to release the directory, so that a crash path does not leave it locked.
11. As a client, I want an emit acknowledged only after its record is fsynced, so that an acknowledged event survives a crash.
12. As a client, I want an emitted event readable by ID and in a list the moment the emit returns, so that I can read my own writes.
13. As a client, I want another client's emitted event readable the moment its emit returns, so that read-your-writes holds across clients.
14. As a client, I want a refused emit to leave nothing behind, so that a refusal has no side effect.
15. As a client, I want a batch's new events to commit together or not at all, so that I never see half a batch.
16. As a client, I want nothing to be visible before it is durable, so that I never read an event that a crash could remove.
17. As a client, I want event time to come from the ledger's event clock and never go back, even across restarts or a clock stepped backwards, so that event time follows commit order.
18. As a client, I want every event in one record to carry that record's event time, so that a batch has one time.
19. As a client, I want an emit whose outcome is unknown answered `Unknown`, so that I know to retry with my idempotency key.
20. As a client, I want a failed ledger to answer `Unavailable` to every call until it is reopened, so that I never act on a ledger in a doubtful state.
21. As a client, I want a keyed retry after reopen to return the original event if it landed and store it once if it did not, so that an unknown outcome is recoverable.
22. As a client, I want reads waiting when the ledger fails to end with `Unavailable`, so that they do not hang.
23. As a client, I want all the emit, batch, idempotency, reading and read-your-writes behavior of the 41 scenarios, so that 0.1 behaves as specified.
24. As an embedder, I want `LocalOperations` to hold no rule of its own, so that the in-process path and later doors behave the same.
25. As a test author, I want a second `LocalOperations` over the same ledger, so that I can act as another client.
26. As a test author, I want a manual clock I can set and step, backwards too, so that time-dependent behavior is deterministic.
27. As an embedder, I want a host clock, so that production event time follows real time.
28. As a maintainer, I want replay to fold each record exactly once, so that reopening never duplicates rows.
29. As a maintainer, I want every record to carry its incarnation token and replay to refuse a mismatch, so that a spliced log is caught.
30. As a maintainer, I want a ledger opened with exactly one environment reference and no environment parameter on any call, so that a request cannot name a second environment.

## Implementation Decisions

- **Module:** the ledger, in the ledger crate, with `LocalOperations`, the host clock and the manual clock beside it. It is the only crate that touches disk, the clock and randomness. It uses `tokio`.
- **Interface:** open (directory, environment reference, clock), submit, read (a query from event rules), close, and a function that mints a fresh environment reference. Everything behind it (writer, fold, read gate) is implementation.
- **Minting:** a UUIDv7 environment ID and a 128-bit token from the OS random source. Event IDs are UUIDv7, minted by the writer and handed to `decide` (D10, D13).
- **Writer:** one serial thread. For each submission: read the event clock, `decide` against the read store, append one record, wait for fsync off the async threads, publish the committed head, acknowledge, fold, then take the next submission (D3, D4, D15). A submission with nothing to append is answered at once.
- **Event clock:** each record's time is the later of the clock reading and the last committed time; the last committed time is recovered on open (D7).
- **Read gate:** a read snapshots the committed head on arrival and waits until the applied position reaches it.
- **Storage:** a segment log directory and a SQLite read store, as in the architecture doc's "What is stored". Offset 0 is the genesis record. The fold writes a record's rows and the new applied position in one transaction and skips a record below the applied position.
- **Open:** the rows of the architecture doc's "Open" table, log first, then read store. The genesis record is the directory's identity (D8, D14). A sealed log is not refused at open (D19).
- **Log use:** `segment-log-core` at exactly 0.2.0 from crates.io (D16), one stream, expected-revision checks, no log idempotency key, the options and error mapping in the architecture doc's "Depends on". The log's own types stay private to this crate.
- **Failure:** an indeterminate append answers `Unknown` and the ledger then refuses every call until reopened; there is no append retry (D11). A fold or read-store failure does the same with `Unavailable`.
- **Stored time in SQLite:** a private wrapper in this crate implements the SQLite conversions over the one `i64` millisecond encoding (D18).
- **`LocalOperations`:** maps each request to a submission or query and each outcome to an answer. Several can share one ledger.
- **Test hooks:** holding the fold and injecting an append or fold fault sit behind the `__fault-points` cargo feature, which no published build enables.
- **Behavior runner:** the `cucumber` crate in an unpublished workspace crate, reading the feature file from the milestone folder (D20).

## Testing Decisions

- A good test drives the ledger through `Operations` or its own open, submit, read and close, and checks what a client or embedder observes. It does not assert on the writer's internals.
- **Seams:** `Operations` (through `LocalOperations`) is the main one and the highest available; `Clock` (the manual clock) is the second. No new seam is added.
- **Behaviors:** all 41 scenarios in the feature file pass in process on a temporary directory with a manual clock; each is listed under "Behavior coverage" below. The runner fails on a skipped or undefined step. Steps call `Operations`, never a URL. Scenarios tagged `@local` use fixtures only this adapter has.
- **Deterministic tests:** every row of the "Deterministic tests" table in the invariants doc: fold-delay (including the first event of a fresh ledger), the clock test, the replay test, the mismatched-open tests, the injected fold failure and the indeterminate append.
- **Minting:** 10,000 minted tokens are distinct.
- **Public-API snapshot and purity checks** from Spec 1 now cover all four crates.
- **Prior art:** the unit tests of Spec 2 for event rules; none for a ledger in this repo. The segment log's own acceptance tests are the nearest model for crash-minded tests.

## Behavior coverage

All 41 scenarios in [`../behaviors/events.feature`](../behaviors/events.feature) are acceptance tests of this spec (story 23). The table maps each one to the stories that own it: the rule's unit tests in [Spec 2](02-event-rules.md), and any ledger story here beyond story 23.

- A scenario is done when it passes under the behavior runner through `LocalOperations`, not when its rule is unit-tested.
- Every ticket cut from this spec names, in its acceptance criteria, the `EVT` IDs it makes pass. Together the tickets name all 41, each exactly once.
- The runner fails on a skipped or undefined step, so a scenario cannot drop out unnoticed. This spec is done when it reports 41 scenarios passed.

| Scenario | Shows | Spec 2 stories | Spec 4 stories |
|---|---|---|---|
| **Emit** | | | |
| EVT-001 | An emit returns a UUID event ID | 1, 41 | 23 |
| EVT-002 | An event reads back with what was sent | 20, 21, 23, 34 | 23 |
| EVT-003 | A non-object payload round-trips | 9 | 23 |
| EVT-004 | No data stores `null` | 8 | 23 |
| EVT-008 | An out-of-range version is refused | 22 | 23 |
| EVT-009 | A version in range reads back as sent | 22 | 23 |
| **Emit gates** | | | |
| EVT-010 | An event needs a name | 2 | 23 |
| EVT-011 | An invalid name is refused | 3, 5 | 23 |
| EVT-012 | Unicode names, names at the limit and near-owned names are accepted | 3, 4, 7 | 23 |
| EVT-013 | Owned names are refused | 6 | 23 |
| EVT-014 | A top-level `$redacted` is refused | 18 | 23 |
| EVT-015 | A nested `$redacted` is accepted | 19 | 23 |
| EVT-016 | Malformed data, metadata or key is refused | 12–17, 20, 23, 45 | 23 |
| EVT-017 | Data over 524,288 bytes is refused; at the limit accepted | 10, 11 | 23 |
| EVT-038 | Metadata over 65,536 bytes is refused; at the limit accepted | 20 | 23 |
| EVT-039 | Data at the edges reads back byte for byte | 10, 15, 16, 17 | 23 |
| **Batch** | | | |
| EVT-020 | A bad item is refused and the others are stored | 2, 29 | 23, 15 |
| EVT-021 | More than 100 items is refused whole | 28 | 23 |
| EVT-022 | A key repeated in one batch returns the earlier item | 31 | 23 |
| EVT-023 | An empty batch answers with no results | 32 | 23 |
| EVT-024 | A batch commits in input order | 33, 36 | 23, 15 |
| EVT-025 | Over 4 MiB of accepted data is refused whole | 30 | 23 |
| **Idempotency** | | | |
| EVT-030 | A repeated key returns the first event | 24 | 23 |
| EVT-031 | Without a key every emit is new | 44 | 23 |
| EVT-032 | A key is not scoped by event name | 25 | 23 |
| EVT-033 | A key holds for as long as its event exists (`@local`) | 26 | 23, 26 |
| EVT-035 | Well-formed keys are accepted | 23 | 23 |
| EVT-036 | Every check runs before the idempotency lookup | 27 | 23 |
| EVT-037 | A malformed key is refused | 23 | 23 |
| **Reading** | | | |
| EVT-040 | An unknown or unparseable ID is not found | 35 | 23 |
| EVT-041 | Events list newest first | 36 | 23 |
| EVT-042 | Events page by cursor | 38, 39 | 23 |
| EVT-043 | A cursor is stable while events arrive | 38 | 23 |
| EVT-045 | A cursor that is not a token is refused | 40 | 23 |
| EVT-046 | The limit defaults to 20 and clamps at 200 | 37 | 23 |
| EVT-053 | An empty environment lists nothing | 39 | 23 |
| EVT-054 | A full last page has no next cursor | 39 | 23 |
| EVT-055 | A cursor from another incarnation is refused (`@local`) | 40 | 23, 4 |
| **Read your writes** | | | |
| EVT-060 | An event is readable as soon as its emit returns | — | 23, 12 |
| EVT-061 | A refused emit leaves nothing behind | 5 | 23, 14 |
| EVT-062 | Another client reads it as soon as the emit returns | — | 23, 13, 25 |

## Out of Scope

- The crash test (Spec 5).
- Power-loss durability, which a later simulator proves.
- More than one environment per process API, cells holding many ledgers, fencing (0.9 and later).
- Credentials (0.2), the HTTP door and remote client (0.3).
- Any performance target.
- Refusing a sealed log at open, until the log library can report one.

## Further Notes

- Invariants proven here: LED-2, LED-6, LED-7, LED-8, LED-11, LED-12, LED-14 (their deterministic and behavior forms). See [`../invariants.md`](../invariants.md).
- Flows, open table, storage and dependencies: [`../architecture.md`](../architecture.md). Scenarios: [`../behaviors/`](../behaviors/README.md).
- The modules diagram and the restart flow diagram still show a "pending view"; D15 replaces it with the read store.
