# Ironflow X foundation

The crates any Ironflow X product links. They commit one environment's events durably, in order, and read them back.

## Language

### Tenancy

**Environment**:
The unit of isolation: one ledger, one directory, one stream of events.

**Incarnation token**:
A random value minted with an environment. An environment recreated under the same ID would get a new one, so the old directory, its records and its cursors are refused; 0.1 has no call that recreates one.

**Environment reference** (`EnvRef`):
An environment's ID with its incarnation token. A ledger is opened with one, and work is never looked up by the ID alone.
_Avoid_: Environment ID, where the reference (ID and token) is meant

### Storage

**Ledger**:
One environment's commit log, read store and the single writer between them.

**Cell**:
One fenceable volume, written by one node, holding many ledgers: the unit of ownership. Not built in 0.1: the `ironflowx-cell` crate is named for it and implements the ledger.

**Submission**:
One call to the ledger's `submit`: a single emit or a batch. It commits at most one record, and none when every item is a duplicate or refused.

**Writer**:
The one thread that takes each submission in turn: decides it, appends its record, acknowledges, then folds.

**Read store**:
`read.db`, the SQLite tables the fold builds from the commit log. Derived state: rebuilt from the log, never trusted over it.

**Fold**:
Writing one record's events and the new applied position to the read store, in one transaction.

**Sealed log**:
A commit log that takes no more appends, as a lossy repair leaves it. It still opens; its first append fails the ledger.

**Record**:
One entry in a ledger's commit log, holding everything one submission commits.
_Avoid_: Log record (that is the segment log's own type)

**Genesis record**:
The first record of a ledger's log, carrying the ledger's identity: format version, environment ID and incarnation token.

**Position**:
A record's place in the commit log, counted from 0.
_Avoid_: Log offset (that is the segment log's own term)

**Committed head**:
One past the last fsynced record: the segment log's watermark.

**Applied position**:
One past the last record the fold has written to the read store.

**Log record**, **log offset**:
The segment log library's own record and offset. They stay inside the ledger and never appear in its interface.

### Events

**Event sequence**:
An event's place in commit order, counted from 1. Each event has one, so a record that holds a batch holds several.

**Event clock**:
The ledger's clock. It never goes back, and it stamps every record after the genesis record.

**Idempotency key**:
An optional client value on an emit. A second emit with a key already used in the environment returns the original event's ID, marked a duplicate, instead of storing another.

### Packaging

**Foundation**:
The crates any Ironflow X product links.

**Facade**:
The `ironflowx` crate, which re-exports the foundation crates.

**Door**:
A way in from outside the process, such as an HTTP server.

### Testing

**Fault point**:
A named place in the commit or recovery path, such as after fsync and before the acknowledgment, where the crash test stops the ledger (`__fault-points`).
_Avoid_: Kill point
