# Spec 2: Event rules

Build-order step 2 of [0.1 Ledger](../README.md). Needs Spec 1.

## Problem Statement

A client that emits an event needs to know, the same way every time and through every door, whether the event is accepted and what is stored. Today nothing decides that. If the checks lived in each door, they would drift apart, and some of them (repeated keys, lone surrogates) can only be made on the JSON text itself.

## Solution

One pure module decides what an event is. Given a submission, the current state and an event-clock reading, it either refuses with a reason or returns what to append. A second function folds an appended record into the read tables, and queries read events back. The module does no I/O, reads no clock and draws no randomness, so the same rules hold for every caller.

## User Stories

1. As a client, I want an event with a valid name to be accepted, so that I can record what happened.
2. As a client, I want an emit with no name refused with "event name is required", so that I know what to fix.
3. As a client, I want a name of 1 to 256 bytes accepted and a longer one refused, counted in UTF-8 bytes, so that the limit is the same in every language.
4. As a client, I want names with Unicode letters accepted, so that I can name events in my own language.
5. As a client, I want a name with an empty segment, whitespace or a control character refused, so that names stay safe to route on.
6. As a client, I want names the engine owns (`ironflowx`, `ironflowx.…`, `$system`, `$system.…`) refused, so that I cannot forge an engine event.
7. As a client, I want names that only resemble an owned name (other case, a longer first segment) accepted, so that the rule takes no more than it needs.
8. As a client, I want an emit without data to store `null`, so that data is always a JSON value.
9. As a client, I want any JSON value as data, not only an object, so that simple payloads need no wrapper.
10. As a client, I want my data stored and returned as the exact bytes I sent, so that number formatting and spacing survive.
11. As a client, I want data over 524,288 bytes as sent refused with "event data exceeds maximum size", so that the limit is clear.
12. As a client, I want data that is not JSON refused, so that nothing unreadable is stored.
13. As a client, I want a repeated key in any object refused, at any depth and after unescaping, so that no two readers disagree about my data.
14. As a client, I want a NUL in any key or string refused, so that data is safe in every store.
15. As a client, I want an unpaired surrogate escape refused, so that stored text is always valid Unicode.
16. As a client, I want data nested more than 127 deep refused, so that no reader overflows on it.
17. As a client, I want a number outside `f64` range refused, so that no reader fails on it.
18. As a client, I want a top-level `$redacted` key in data refused whatever its value, so that I cannot forge a redaction marker.
19. As a client, I want a nested `$redacted` key accepted, so that the rule takes no more than it needs.
20. As a client, I want metadata to be absent or a JSON object of at most 65,536 bytes, under the same text rules as data, so that metadata is bounded and well formed.
21. As a client, I want an absent version to store 1, so that I need not set one.
22. As a client, I want a version from 1 to 2,147,483,647 stored as sent, and anything else refused, so that zero never silently means "unset".
23. As a client, I want an idempotency key of 1 to 256 bytes with no NUL accepted, and an empty or longer one refused, so that a key is always usable.
24. As a client, I want a repeated key to return the first event and store nothing, so that a retry is safe.
25. As a client, I want a key to apply across event names, so that one key names one event in the environment.
26. As a client, I want a key to hold for as long as its event exists, so that a late retry is still safe.
27. As a client, I want every check to run before the idempotency lookup, so that the same bad request is refused the same way whether or not its key was seen.
28. As a client, I want a batch of more than 100 items refused whole with "batch size {n} exceeds maximum of 100", so that the limit is clear.
29. As a client, I want a bad item in a batch refused on its own while the others are stored, so that one mistake does not lose the rest.
30. As a client, I want a batch whose accepted data and metadata total more than 4 MiB refused whole, so that one record stays bounded.
31. As a client, I want a key repeated inside one batch to return the earlier item's event, so that a batch behaves like its items emitted in order.
32. As a client, I want an empty batch to answer with no results, so that it is not an error.
33. As a client, I want a batch's new events to take event sequences in input order, so that commit order matches what I sent.
34. As a client, I want to read an event by ID with everything I sent and its event time, so that I can verify it.
35. As a client, I want an unknown or unparseable ID answered "event not found", so that both cases look the same.
36. As a client, I want events listed newest first by event sequence, so that two events never tie.
37. As a client, I want a page limit that defaults to 20 and clamps at 200, so that a page is always bounded.
38. As a client, I want a cursor that continues from where the last page ended even while new events arrive, so that paging is stable.
39. As a client, I want the last page to carry no cursor, even when it is full, so that I know when to stop.
40. As a client, I want a cursor that does not decode, or that names another incarnation, refused as invalid, so that a stale cursor cannot read the wrong environment.
41. As the ledger, I want `decide` to draw an event ID only for each new event, from an iterator I supply, so that event rules stay pure.
42. As the ledger, I want `fold` to apply one record to the read tables, so that the read store is derived only from the log.
43. As a maintainer, I want event rules to hold every check, default and limit, so that an adapter over them holds no rule.

## Implementation Decisions

- **Module:** event rules, in the event-rules crate. Pure (LED-15): it depends on core and `serde_json` only.
- **Interface:** `decide` (submission, view of current state, event-clock time, iterator of fresh event IDs → decision or refusal), `fold` (record → read tables), and queries (event by ID; a page of events). The shapes are in the architecture doc.
- **Order of `decide`:** batch count, then per-item rules, then batch bytes over the items that passed, then the idempotency lookup in input order (a key seen earlier in the same batch counts), then one ID per new event (D5, D9, D10).
- **Rules:** exactly the table in the architecture doc's "Event rules" section. Name character classes are Rust's `char::is_whitespace` and `char::is_control`; owned names match in exact case.
- **JSON checking:** one `serde_json` pass with a visitor that deserializes every key and string, and errors on a repeated key, a NUL or a number out of range (D12). No second parser. Data and metadata are kept as the bytes sent.
- **State access:** `decide` reads through a view trait and `fold` writes through a tables trait, so the crate never names SQLite. The ledger supplies the real ones (Spec 4).
- **Refusals:** a refusal carries the `Invalid` kind and a message. Three messages are contract here: "event name is required", "event data exceeds maximum size", "batch size {n} exceeds maximum of 100". "event not found" is contract for the by-ID query.
- **Event sequence:** starts at 1 and is assigned in input order within a record.
- **Cursor:** encodes the incarnation token and the last event sequence returned. It is an encoding, not a signature.
- **Left to the implementer:** whether the record's binary encoding lives in this crate or in the ledger crate. Either is fine while this crate stays pure and the format is length-prefixed with a format version.

## Testing Decisions

- A good test gives `decide` a submission and a state and checks the decision or refusal; it gives `fold` a record and checks what the queries then return. It does not assert on internal parsing steps.
- **Seam:** the view and tables traits. Tests use a small in-memory implementation; the ledger's SQLite one is tested in Spec 4. No other seam is added.
- **Coverage:** one unit test per row of the rules table, plus the `decide` ordering (a refused request with a known key is still refused), batch partial refusal, in-batch key reuse, and cursor encode, decode and refusal.
- **Fold:** folding the same record twice leaves rows unchanged when the caller skips applied positions; the skip itself is the ledger's (Spec 4).
- The behavior scenarios (EVT-001 to EVT-062) sample the same rules end to end and run in Spec 4. These unit tests exist so the rules are proven before the ledger is built.
- **Prior art:** none in this repo.

## Out of Scope

- Reading the clock, minting IDs, appending and fsync: the ledger (Spec 4).
- Owned-name emits by a verified emitter (0.2), filters, totals and paging back (0.4), keys per environment (0.9), event schemas (0.12 onwards).
- Rejecting wildcard characters in names: not carried until a release uses patterns.

## Further Notes

- Invariants: LED-3, LED-4, LED-5, and the "event rules read no clock" half of LED-2. See [`../invariants.md`](../invariants.md).
- Rules and decisions: [`../architecture.md`](../architecture.md#event-rules). Scenarios: [`../behaviors/events.feature`](../behaviors/events.feature).
