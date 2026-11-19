# Events in release 0.1: emit, batch emit, idempotency, reading events back by
# ID and by page, and read your writes, through Operations with no credential.
# Conventions, adapters and what is out of scope: README.md. The rules these
# scenarios sample: architecture.md, "Event rules".

Feature: Emitting events and reading them back

  Background:
    Given a fresh environment
    And a client

  # ---- Emit ----

  @EVT-001 @release-0.1
  # go: api/proto/ironflow/v1/ironflow.proto TriggerResponse
  # go: internal/server/connect/handler_test.go TestHandler_Trigger_IdempotencyKeyEmpty
  # go: internal/eventtrigger/helper.go Emit (uuid.New)
  # go: docs/reference/api/rest-api.md ("runIds is omitted")
  Scenario: Emitting an event returns its ID
    When the client emits event "order.placed" with data {"order_id": "o-1"}
    Then the emit succeeds with a UUID event ID

  @EVT-002 @release-0.1
  # go: tests/integration/api_responses_test.go TestEventsAPIResponseFormat
  # go: tests/integration/api_responses_test.go TestEventDataFieldNotPayload
  # go: docs/reference/api/rest-api.md "GET /events/{id}" (idempotency_key)
  # go: internal/server/event_version_test.go TestTriggerEvent_OmittedVersionMeansOne
  Scenario: An emitted event reads back with what was sent
    When the client emits event "order.placed" with data {"order_id": "o-1"}, metadata {"trace": "t-1"} and idempotency key "k-1"
    And the client reads that event by ID
    Then it has name "order.placed", data {"order_id": "o-1"}, metadata {"trace": "t-1"}, version 1 and idempotency key "k-1"

  @EVT-003 @release-0.1
  # go: internal/server/event_emit_connect_migration_test.go TestEventEmitConnectMigration
  # go: api/proto/ironflow/v1/ironflow.proto TriggerRequest data_value
  Scenario Outline: A non-object payload round-trips
    When the client emits event "value.set" with data value <value>
    And the client reads that event by ID
    Then its data is <value>

    Examples:
      | value       |
      | false       |
      | ["payload"] |
      | null        |
      | "text"      |
      | 42          |

  @EVT-004 @release-0.1
  # go: internal/server/event_emit_connect_migration_test.go TestEventEmitConnectMigration
  Scenario: An event emitted without data stores null
    When the client emits event "ping" with no data
    And the client reads that event by ID
    Then its data is null

  @EVT-008 @release-0.1
  # go: internal/server/event_version_test.go TestTriggerEvent_NegativeVersionIs400
  # go: internal/server/event_version_test.go TestTriggerEvent_VersionAboveInt32Is400
  # go: docs/adr/0068 Amendment (0 coerced to 1; differs: Option makes absence explicit, so 0 is refused)
  Scenario Outline: An out-of-range version is refused
    When the client emits event "order.placed" with version <version>
    Then the emit is refused as invalid
    And no event is stored

    Examples:
      | version    |
      | -1         |
      | 0          |
      | 2147483648 |

  @EVT-009 @release-0.1
  # go: internal/server/event_version_test.go TestTriggerEvent_VersionIsCallerSupplied
  Scenario Outline: A version in range reads back as sent
    When the client emits event "order.placed" with version <version>
    And the client reads that event by ID
    Then it has version <version>

    Examples:
      | version    |
      | 1          |
      | 3          |
      | 2147483647 |

  # ---- Emit gates ----

  @EVT-010 @release-0.1
  # go: internal/server/validation_test.go TestTriggerEventValidation_MissingEventName
  # go: internal/server/validation_test.go TestTriggerEventValidation_EmptyEventName
  Scenario: An event needs a name
    When the client emits an event with no name
    Then the emit is refused as invalid with "event name is required"
    And no event is stored

  @EVT-011 @release-0.1
  # go: internal/eventtrigger/helper_test.go TestEmit_RejectsInvalidEventName
  # go: internal/server/connect/handler_test.go TestHandler_Trigger_UntrustedEventName
  # go: internal/pubsub/codec.go ValidateEventName
  Scenario Outline: An invalid event name is refused and nothing is stored
    When the client emits event <name>
    Then the emit is refused as invalid
    And no event is stored

    Examples:
      | name                        | why                      |
      | a name of 257 ASCII letters | over the length limit    |
      | "order placed"              | whitespace               |
      | "order\u0000placed"         | control character        |
      | "a..b"                      | empty segment            |
      | ".a"                        | empty segment            |
      | "a."                        | empty segment            |
      | "order\u00a0placed"         | Unicode whitespace       |
      | a name of 129 "é"           | 258 bytes, over the limit |

  @EVT-012 @release-0.1
  # go: internal/eventtrigger/helper_test.go TestEmit_AcceptsBoundaryEventName
  # go: docs/reference/reserved-names.md "Roots" (event roots match exact case)
  Scenario Outline: Unicode names, names at the limit and other-case owned roots are accepted
    When the client emits event <name>
    Then the emit succeeds

    Examples:
      | name                  |
      | "订单.placed"          |
      | a name of 256 ASCII letters |
      | a name of 128 "é"     |
      | "Ironflowx.run.done"  |
      | "ironflowxy.done"     |
      | "$System.tick"        |
      | "$custom.tick"        |

  @EVT-013 @release-0.1
  # go: internal/server/connect/handler_test.go TestHandler_Trigger_OwnedEventNames
  # go: docs/reference/reserved-names.md
  Scenario Outline: Names the engine owns are refused
    When the client emits event <name>
    Then the emit is refused as invalid
    And no event is stored

    Examples:
      | name                  |
      | "ironflowx"           |
      | "ironflowx.run.done"  |
      | "$system"             |
      | "$system.tick"        |

  @EVT-014 @release-0.1
  # go: docs/reference/reserved-names.md "Payload markers"
  # go: internal/server/connect/payload_marker_test.go TestUserDoorsRejectRedactedMarker
  Scenario Outline: A top-level redaction marker in the payload is refused, whatever its value
    When the client emits event "order.placed" with data <data>
    Then the emit is refused as invalid
    And no event is stored

    Examples:
      | data                            |
      | {"$redacted": true}             |
      | {"$redacted": false}            |
      | {"a": 1, "$redacted": null}     |

  @EVT-015 @release-0.1
  # go: docs/reference/reserved-names.md "Payload markers"
  # go: internal/server/connect/payload_marker_test.go TestUserDoorsRejectRedactedMarker
  Scenario: A nested redaction marker is accepted
    When the client emits event "order.placed" with data {"inner": {"$redacted": true}}
    Then the emit succeeds

  @EVT-016 @release-0.1
  # go: internal/reserved/reserved_test.go
  # go: _internal/invariants/emit-gates-every-door.md (ADR 0175 event-data check)
  # go: docs/adr/0175-require-unique-keys-in-event-data.md
  # go: internal/server/connect/payload_marker_test.go TestEventDoorsRejectDuplicateKeyOnTheWire
  # go: internal/server/connect/payload_marker_test.go TestEventDoorsRejectNulOnTheWire
  # go: none for "not JSON", depth and metadata shape: Ironflow X takes JSON text (architecture.md, Event rules)
  Scenario Outline: Malformed event data is refused
    When the client emits event "order.placed" whose <part> is <text>
    Then the emit is refused as invalid
    And no event is stored

    Examples:
      | part            | text                               | why                           |
      | data            | {"a": 1, "a": 2}                   | duplicate key                 |
      | data            | {"x": {"a": 1, "a": 2}}            | duplicate key, nested         |
      | data            | {"a": 1, "\u0061": 2}              | keys equal after unescaping   |
      | data            | "x\u0000y"                         | NUL in a top-level string     |
      | data            | {"k": "x\u0000y"}                  | NUL in a nested string        |
      | data            | {"k\u0000": 1}                     | NUL in a key                  |
      | data            | {"k": "\ud800"}                    | unpaired high surrogate       |
      | data            | {"k": "\udc00"}                    | lone low surrogate            |
      | data            | {"k": 1                            | not JSON                      |
      | data            | 128 nested arrays                  | deeper than 127               |
      | data            | 1e400                              | number out of range           |
      | data            | ["ok", "\ud800"]                   | unpaired surrogate in an array |
      | metadata        | {"t": 1, "t": 2}                   | duplicate key                 |
      | metadata        | ["t"]                              | not an object                 |
      | metadata        | {"t": "x\u0000"}                   | NUL                           |
      | metadata        | {"t": "\udc00"}                    | lone low surrogate            |
      | idempotency key | "k\u0000"                          | NUL                           |

  @EVT-017 @release-0.1
  # go: internal/server/connect/handler.go validatePayloadSize
  # go: internal/payloadsize Validate
  # go: internal/server/connect/handler_test.go TestTrigger_OversizedPayload
  Scenario: Data over 524,288 bytes as sent is refused, and data at the limit is accepted
    When the client emits event "blob.saved" with data {"blob": "<ASCII a repeated>"} sent as exactly 524,289 bytes
    Then the emit is refused as invalid, and the message contains "event data exceeds maximum size"
    And no event is stored
    When the client emits event "blob.saved" with data {"blob": "<ASCII a repeated>"} padded with spaces to exactly 524,289 bytes
    Then the emit is refused as invalid
    When the client emits event "blob.saved" with data {"blob": "<ASCII a repeated>"} sent as exactly 524,288 bytes
    Then the emit succeeds

  @EVT-038 @release-0.1
  # go: internal/payloadsize Validate (data only; differs: Ironflow X bounds metadata too)
  Scenario: Metadata over 65,536 bytes as sent is refused, and metadata at the limit is accepted
    When the client emits event "order.placed" with metadata {"t": "<ASCII a repeated>"} sent as exactly 65,537 bytes
    Then the emit is refused as invalid
    And no event is stored
    When the client emits event "order.placed" with metadata {"t": "<ASCII a repeated>"} sent as exactly 65,536 bytes
    Then the emit succeeds

  @EVT-039 @release-0.1
  # go: internal/server/event_emit_connect_migration_test.go TestEventEmitConnectMigration (round trip)
  # go: none for depth and byte-exact text: Ironflow X stores the JSON text as sent (architecture.md, Event rules)
  Scenario Outline: Data at the edges is accepted and reads back byte for byte
    When the client emits event "edge.case" with data <data>
    And the client reads that event by ID
    Then its data is exactly the text sent

    Examples:
      | data                       |
      | 127 nested arrays          |
      | {"k": "\ud83d\ude00"}      |
      | {"n": 1.10, "big": 123456789012345678901234567890} |
      | {  "spaced" :  true  }     |

  # ---- Batch ----

  @EVT-020 @release-0.1
  # go: internal/server/connect/handler_test.go TestHandler_TriggerBatch_ReportsItemError
  # go: docs/reference/js-sdk/core.md "triggerBatch" (eventId empty when the item failed)
  # go: internal/server/connect/handler.go Trigger ("event name is required")
  Scenario: A batch reports a bad item and still emits the others
    When the client emits a batch of "a.ok", an event with no name, and "b.ok"
    Then the batch call succeeds
    And the second item is refused as invalid and has no event ID
    And events "a.ok" and "b.ok" are stored

  @EVT-021 @release-0.1
  # go: internal/server/connect/handler.go TriggerBatch (MaxBatchSize)
  # go: internal/server/connect/handler_test.go TestTriggerBatch_ExceedsMaxBatchSize
  Scenario: A batch holds at most 100 events
    When the client emits a batch of 101 events
    Then the batch is refused as invalid with "batch size 101 exceeds maximum of 100"
    And no event is stored

  @EVT-022 @release-0.1
  # go: internal/server/connect/handler_test.go TestHandler_Trigger_IdempotencyKeyDuplicate (across calls; within one batch: Ironflow X decision D9)
  Scenario: A key repeated inside one batch returns the earlier item's event
    When the client emits a batch of "a.ok" with idempotency key "k-1" and "b.ok" with idempotency key "k-1"
    Then both items return the same event ID
    And the second item is a duplicate
    And only "a.ok" is stored

  @EVT-023 @release-0.1
  # go: internal/server/connect/handler_test.go TestHandler_TriggerBatch_EmptyEvents
  Scenario: An empty batch answers with no results
    When the client emits a batch of no events
    Then the batch call succeeds with no results
    And no event is stored

  @EVT-024 @release-0.1
  # go: docs/reference/js-sdk/core.md "triggerBatch" (results in input order)
  Scenario: A batch's events commit in input order
    When the client emits a batch of "b.1", "b.2" and "b.3"
    And the client lists events
    Then they come back as "b.3", "b.2", "b.1"

  @EVT-025 @release-0.1
  # go: none: Ironflow X bounds one record (architecture.md, Event rules)
  Scenario: A batch whose accepted data totals over 4 MiB is refused whole
    When the client emits a batch of 9 events, each with data of 524,288 bytes
    Then the batch is refused as invalid
    And no event is stored

  # ---- Idempotency ----

  @EVT-030 @release-0.1
  # go: internal/server/connect/handler_test.go TestHandler_Trigger_IdempotencyKeyDuplicate
  # go: internal/eventtrigger/characterization_test.go TestCharCand3_RESTEmit_IdempotentDup_SameRunIDs
  Scenario: Repeating an idempotency key returns the first event and stores nothing new
    When the client emits event "order.placed" with idempotency key "order-1"
    And the client emits event "order.placed" with idempotency key "order-1" again
    Then both emits return the same event ID
    And one event is stored

  @EVT-031 @release-0.1
  # go: internal/server/connect/handler_test.go TestHandler_Trigger_IdempotencyKeyEmpty
  Scenario: Without a key every emit is a new event
    When the client emits event "order.placed" twice without an idempotency key
    Then the emits return different event IDs
    And two events are stored

  @EVT-032 @release-0.1
  # go: docs/reference/cli/commands/emit.md (never reuse a key for another event)
  # go: docs/adr/0007 (unique index on environment and key)
  # go: internal/eventtrigger/helper.go Emit (GetEventByIdempotencyKey(envID, key))
  Scenario: A key is not scoped by event name
    When the client emits event "order.placed" with idempotency key "k-1"
    And the client emits event "order.cancelled" with idempotency key "k-1"
    Then the second emit returns the first event's ID
    And only "order.placed" is stored

  @EVT-033 @release-0.1 @local
  # go: docs/adr/0132 ("a database UNIQUE (environment_id, idempotency_key) dedups them for good")
  Scenario: A key holds for as long as its event exists
    Given the client emitted event "order.placed" with idempotency key "order-1" 30 days ago
    When the client emits event "order.placed" with idempotency key "order-1"
    Then the emit returns the original event ID

  @EVT-035 @release-0.1
  # go: internal/reserved/reserved_test.go TestIdempotencyKey
  Scenario Outline: Idempotency keys that are not malformed are accepted
    When the client emits event "order.placed" with idempotency key <key>
    Then the emit succeeds

    Examples:
      | key               |
      | "order-1"         |
      | a key of 256 ASCII letters |
      | "literal \\\\u0000" |

  @EVT-036 @release-0.1
  # go: internal/eventtrigger/helper.go Emit (name, reserved and data checks before GetEventByIdempotencyKey)
  # go: docs/adr/0175 Decision (funnel refuses before its idempotency lookup)
  Scenario Outline: Every check runs before the idempotency lookup
    Given the client emitted event "order.placed" with idempotency key "order-1"
    When the client emits <request> with idempotency key "order-1"
    Then the emit is refused as invalid
    And one event is stored

    Examples:
      | request                                        |
      | event "order placed"                           |
      | event "order.placed" with data {"a": 1, "a": 2} |
      | event "order.placed" with version -1           |

  @EVT-037 @release-0.1
  # go: internal/server/connect/handler_test.go TestHandler_Trigger_IdempotencyKeyEmpty (differs: "" meant no key; Option makes absence explicit)
  Scenario Outline: A malformed idempotency key is refused
    When the client emits event "order.placed" with idempotency key <key>
    Then the emit is refused as invalid
    And no event is stored

    Examples:
      | key                        |
      | ""                         |
      | a key of 257 ASCII letters |

  # ---- Reading events ----

  @EVT-040 @release-0.1
  # go: internal/server/coverage_bump_test.go TestHandleGetEvent_AllPaths
  Scenario Outline: An unknown or unparseable event ID is not found
    When the client reads event <id>
    Then the read is refused as not found with "event not found"

    Examples:
      | id                                     |
      | "019a8f3c-7e21-7b4a-9c3d-5e6f7a8b9c0d" |
      | "not-a-uuid"                           |

  @EVT-041 @release-0.1
  # go: docs/reference/api/rest-api.md "Events"
  # go: internal/store/sqlite_event_keyset_test.go TestSQLiteStore_ListEvents_KeysetForward_StableUnderInserts
  Scenario: Events list newest first, in commit order
    Given the client emitted events "e.1", "e.2" and "e.3" in that order
    When the client lists events
    Then they come back as "e.3", "e.2", "e.1"

  @EVT-042 @release-0.1
  # go: internal/server/projection_list_rest_test.go TestListEvents_Pagination
  # go: internal/store/sqlite_event_keyset_test.go TestSQLiteStore_ListEvents_HasNextHasPrev
  Scenario: Events page by cursor
    Given the client emitted 5 events
    When the client lists events with a limit of 2
    Then the page holds 2 events and has a next page
    When the client follows the next cursor with a limit of 3
    Then the page holds 3 events and has no next page

  @EVT-043 @release-0.1
  # go: internal/store/sqlite_event_keyset_test.go TestSQLiteStore_ListEvents_KeysetForward_StableUnderInserts
  Scenario: A cursor is stable while new events arrive
    Given the client emitted 4 events and listed the first page of 2
    When the client emits 3 more events and follows the next cursor
    Then the page holds the next 2 of the original events

  @EVT-045 @release-0.1
  # go: internal/server/projection_list_rest_test.go TestListEvents_CursorErrors
  # go: internal/store/sqlite_event_keyset_test.go TestSQLiteStore_ListEvents_InvalidCursor
  Scenario: A cursor that is not a token is refused
    When the client lists events with a cursor that is not a token
    Then the list is refused as invalid

  @EVT-046 @release-0.1
  # go: docs/reference/api/rest-api.md "Events"
  Scenario Outline: The page limit defaults to 20 and clamps at 200
    Given the client emitted 250 events
    When the client lists events with <limit>
    Then the page holds <count> events

    Examples:
      | limit       | count |
      | no limit    | 20    |
      | limit 500   | 200   |

  @EVT-053 @release-0.1
  # go: internal/store/sqlite_event_keyset_test.go TestSQLiteStore_ListEvents_HasNextHasPrev
  Scenario: An empty environment lists no events and no next page
    When the client lists events
    Then the page holds 0 events and has no next page

  @EVT-054 @release-0.1
  # go: internal/store/sqlite_event_keyset_test.go TestSQLiteStore_ListEvents_HasNextHasPrev
  Scenario: The last page has no next cursor, even when it is full
    Given the client emitted 4 events
    When the client lists events with a limit of 2
    And the client follows the next cursor with a limit of 2
    Then the page holds 2 events and has no next page

  @EVT-055 @release-0.1 @local
  # go: internal/store/sqlite_event_keyset_test.go TestSQLiteStore_ListEvents_InvalidCursor
  Scenario: A cursor from another incarnation of the environment is refused
    Given a cursor issued by an environment with the same ID and another incarnation token
    When the client lists events with that cursor
    Then the list is refused as invalid

  # ---- Read your writes ----

  @EVT-060 @release-0.1
  # go: internal/eventtrigger/helper.go Emit (CreateEvent commits before the response)
  # go: internal/server/event_emit_connect_migration_test.go TestEventEmitConnectMigration
  Scenario: An emitted event is readable as soon as the emit returns
    When the client emits event "order.placed"
    Then reading that event by ID right away finds it
    And listing events right away includes it

  @EVT-061 @release-0.1
  # go: internal/eventtrigger/helper_test.go TestEmit_RejectsInvalidEventName (ListEvents returns 0)
  Scenario: A refused emit leaves nothing behind
    When the client emits event "a..b"
    Then listing events returns none

  @EVT-062 @release-0.1
  # go: internal/eventtrigger/helper.go Emit (CreateEvent commits before the response)
  Scenario: Another client reads an event as soon as its emit returns
    Given another client
    When the client emits event "order.placed"
    Then the other client reading that event by ID right away finds it
    And the other client listing events right away includes it
