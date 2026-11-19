# Ironflow X foundation

The crates any Ironflow X product links. They commit one environment's events durably, in order, and read them back.

## Language

### Tenancy

**Environment**:
The unit of isolation: one ledger, one directory, one stream of events.

**Incarnation token**:
A random value minted when an environment is created. A recreated environment with the same ID gets a new one.

### Storage

**Ledger**:
One environment's commit log, read store and the single writer between them.

**Cell**:
One fenceable volume, written by one node, holding many ledgers: the unit of ownership.

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
The ledger's clock. It never goes back, and it stamps every record.

### Packaging

**Foundation**:
The crates any Ironflow X product links.

**Facade**:
The `ironflowx` crate, which re-exports the foundation crates.

**Door**:
A way in from outside the process, such as an HTTP server.
