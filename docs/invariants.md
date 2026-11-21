# Invariants

The properties release 0.1 must prove. A behavior shows one example; an invariant holds for every input. The **S** properties are proven by the crash test below in 0.1, and by a seeded fault simulator in a later release.

**Form:** **T** a type or API makes the violation unrepresentable · **U** a unit or integration test · **S** a property checked under faults · **B** a behavior scenario · **C** a CI check, which must reject a violating fixture.

| ID | Property | Rust form | Form | Proven in 0.1 by |
|---|---|---|---|---|
| LED-1 | Every time compared with event time comes from the event clock | `SequencedTime` implements no comparison with `Timestamp` or `std::time`. Its one constructor is `#[doc(hidden)]`, and clippy `disallowed-methods` bans it in every crate but `ironflowx-cell` (D17) | T, C | Compile-fail tests: compared with a `Timestamp`; compared with `SystemTime`. The lint, with a fixture it must reject |
| LED-2 | Event time never comes from an ambient clock and never goes back, across restarts too | Event rules read no clock; each record's time is the later of the clock and the last committed time | C, U, S | LED-15's checks; the clock test below; crash test |
| LED-3 | An emit with a name the engine owns is refused | Inside event rules, which every caller reaches through `emit` | B | EVT-012, EVT-013 |
| LED-4 | An emit with a reserved payload marker at the top level is refused | As LED-3 | B | EVT-014, EVT-015 |
| LED-5 | Event data and metadata are valid UTF-8 with unique keys, no NUL, and paired surrogates at any depth; an idempotency key has no NUL | `emit` takes bytes (D22), and event rules parse them with the visitor of D12 | B | EVT-016, EVT-039 |
| LED-6 | Each incarnation of an environment has a random token, and the ledger's identity includes it | `IncarnationToken` has private fields; it is minted only by `new_env_ref`, from `getrandom`, and otherwise only parsed from its text form. The genesis record carries it | T, U | A public-API check that no other constructor exists; 10,000 minted tokens are distinct; a mismatched open refuses |
| LED-7 | Every request names exactly one environment | A `Ledger` is opened with exactly one `EnvRef`; `submit`, `read` and `Operations` have no environment parameter | T | The type |
| LED-8 | Every record carries its incarnation token; open and replay refuse a mismatch | Part of the record envelope; the genesis record is checked on every open | U | A log whose genesis names another token refuses; a log with one later record carrying another token refuses |
| LED-9 | Work is never looked up by a bare environment ID | No public function in the 0.1 crates takes an `EnvironmentId` without its token | T, C | A public-API check over the four crates |
| LED-10 | An environment ID is opaque | `EnvironmentId` is a validated UUIDv7 newtype in `ironflowx-core`; parsing refuses anything else. The parent-as-a-field half of this rule arrives with tenancy in 0.2 | T, U | Parse tests: a UUIDv4, a path-like string and the empty string are refused |
| LED-11 | Everything an operation changes in the ledger commits in one submit or not at all, and nothing is visible before it is durable | One record per submission; reads see only applied positions, and the fold applies only fsynced records | B, S | EVT-020, EVT-024, EVT-061; crash test |
| LED-12 | Replaying the log folds each record exactly once | The fold skips a record below the applied position, in the same transaction that advances it | U, S | The replay test below; crash test |
| LED-13 | A timestamp has one integer encoding | `Timestamp::from_millis` and `as_millis` (`i64`, whole milliseconds) are its only conversions; its fields are private. The read store and the log record both go through them (D18) | T, U | Round-trip and ordering test over the extremes; a public-API check for other conversions |
| LED-14 | A read sees every write acknowledged before it arrived, from any client | The writer publishes the committed head before it acknowledges; a read waits for the head it saw on arrival | U, B, S | Fold-delay test; EVT-060, EVT-062; crash test |
| LED-15 | Pure crates do no I/O: no async runtime, no I/O crate, no ambient clock or randomness | For `ironflowx-events` and `ironflowx-core`: a dependency allowlist over `cargo tree` for that crate alone, and clippy `disallowed-methods` and `disallowed-types` for `SystemTime::now`, `Instant::now`, `std::fs`, `std::net`, `std::thread` and `getrandom` | C | Both checks, each with a fixture they must reject |

A public-API check compares `cargo-public-api` output for the four crates with a committed snapshot; any difference fails (D21).

## Deterministic tests

| Test | Proves |
|---|---|
| Fold-delay: a test hook holds the fold; client A emits; client B reads by ID and lists; release the hook. Repeat for the very first event of a fresh ledger | LED-14: B's read waits, then finds the event |
| `ManualClock` at t: emit, the event's time is t. Step +1 h: emit, t + 1 h. Step −2 h: emit, still t + 1 h. Close, reopen, emit: still t + 1 h | LED-2 |
| Fold records up to k, then feed records at or below k again: rows and applied position unchanged. Then delete `read.db`, reopen: identical rows | LED-12 |
| Open a directory with another `EnvRef`, with a genesis of another format, and after deleting an empty ledger's `read.db` | LED-6, LED-8, D14 |
| Fold failure injected after an acknowledgment | Waiting reads end `Unavailable`; reopen applies the record |
| An append that returns `Indeterminate`, through the fault hook: the call answers `Unknown`, the next call `Unavailable`; reopen; a keyed retry returns the original event if it landed, else stores it once | D11, the Errors table |

## Crash test

A parent process drives a child process that owns the ledger, over a pipe, with keyed and unkeyed emits and mixed batches. The child is built with the `__fault-points` cargo feature, which no published build enables (a CI check); the parent reads `read.db` directly, read-only, to see sequences and the applied position.

- **Named kill points**, each run deterministically first, the child and parent meeting at a barrier: before the append, after fsync and before the acknowledgment, after the acknowledgment and before the fold, mid-fold, and during open. Then seeded random kills; the seed is printed and replays the run.
- **The parent records** each acknowledged event ID and key, the cursor of its last list, and each request with no answer. Every new event carries a unique marker in its metadata, so the parent can find it without its ID; it also knows each item's expected outcome.
- **After each kill** it reopens twice and asserts:
  - every acknowledged ID reads back, and every acknowledged key returns its original ID;
  - the new events of an unanswered request are all present or all absent; its duplicates and refusals store nothing;
  - event sequences have no gap and no repeat, and event times never decrease while the seed steps the `ManualClock` both ways;
  - both reopens give the same events, keys, sequences and applied position;
  - the recorded cursor still pages from the same place.

Every PR runs the named kill points and 100 seeded kills, on Linux and macOS. 0.1 has no longer scheduled run; a longer soak is run by hand before publishing.

A kill keeps the operating system's page cache, so this test proves crash durability, not power-loss durability. The simulator proves the second in a later release.
