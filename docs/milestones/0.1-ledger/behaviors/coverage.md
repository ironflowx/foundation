# 0.1 Ledger: events coverage sweep

> Moved here with release 0.1. Scenarios cited as EVT-nnn are in [`events.feature`](events.feature). An item marked "deferred to 0.N" is harvested again when that release is drafted (plan decisions 71, 89).

- **Date:** 2026-11-17
- **Spec:** [`events.feature`](events.feature) for 0.1; later-release items are marked with the release that harvests them.
- **Swept:** `tests/conformance/` (`scenarios_test.go`, `harness_test.go`, `known_gaps.json`, `fixtures/`, `README.md`); public docs under `docs/` (reference, explanation, how-to guides; `docs/blog/` excluded as dated posts, not reference claims); ADRs 0007, 0068, 0128, 0129, 0130, 0132, 0133, 0153, 0167, 0175.
- **Rule (decision 71):** every conformance scenario and every public-docs claim in the release's scope maps to a behavior ID, or to a line marked moved, removed or deferred with its reason. An unmapped item blocks handoff.
- **Release under sweep:** 0.1. A later-release item maps to its tagged scenario or header line, or to the release that owns it.

## 1. Conformance (`tests/conformance`)

`known_gaps.json` is `{}`, and the fixtures emit nothing. Every scenario starts its runs with `h.trigger`/`h.emit`/`h.emitN`/`h.triggerV` (Connect `Trigger`, `harness_test.go:241-429`). So the emit step in each of them is EVT-001, and the scenario itself belongs to the capability named below.

| Source | Scenario | Maps to |
|---|---|---|
| `scenarios_test.go:113` `run-memoize` | trigger starts a run; steps memoize | Out of scope: cap 1, 0.4 (emit step: EVT-001) |
| `:127` `sleep-resume` | sleep then resume | Out of scope: 0.5 |
| `:138` `wait-event-hit` | emitted event wakes `waitForEvent`, output is the event data | Out of scope: 0.6 (emit step: EVT-001) |
| `:155` `wait-event-timeout` | wait times out | Out of scope: 0.6 |
| `:166` `parallel-ids`, `:267` `map-ids`, `:557` `parallel-multi-yield`, `:583` `yield-tail-over-500` | branch step IDs | Out of scope: cap 1, 0.4-0.6 |
| `:189` `non-retryable` | NonRetryableError fails after 1 attempt | Out of scope: 0.4 (roadmap example EXE-014) |
| `:202` `invoke-sync`, `:216` `invoke-async`, `:234` `invoke-child-fails`, `:249` `invoke-timeout` | child invoke | Out of scope: 0.6 |
| `:294` `saga-compensate`, `:326` `saga-retryable-no-comp` | compensation order | Out of scope: cap 1 (saga has no release row yet) |
| `:346` `upcast` | trigger with `version: 1`, upcaster runs | Out of scope: 0.12 (version field: deferred to 0.4) |
| `:361` `projection-managed`, `:371` `projection-external`, `:720-736` `projection-stream-cut`, `-save-fails`, `-save-response-dropped`, `-ack-fails` | `emitN` feeds a projection | Out of scope: 0.13 (emit step: EVT-001) |
| `:374` `agent-spawn`, `:388` `agent-memory`, `:420` `agent-expose-mcp`, `:657` `agent-crash-resume` | agent APIs | Out of scope: agents (no release row) |
| `:403` `agent-approve` | emitted approval event resolves `approve()` | Out of scope: 0.6 (`waitForEvent` sugar, ADR 0128) |
| `:443` `crash-resume` | run resumes after worker kill | Out of scope: 0.5 |
| `:468` `lease-outlives-drain`, `:682` `fencing`, `:599` `checkpoint-fail-then-yield`, `:622` `checkpoint-response-lost` | pull leases, fencing, checkpoints | Out of scope: 0.7 |

No conformance scenario is in 0.1 scope. No event-only scenario exists to map.

## 2. Public docs

### `docs/reference/api/rest-api.md`

| Source | Claim | Maps to |
|---|---|---|
| "Errors" (l.101) | retry a write only with an idempotency key | EVT-030 |
| "Errors" table (l.89-98) | error envelope fields, `retryable` | Out of scope: 0.3 (wire) |
| "Emit an event" (l.149) | `Emit` RPC; `Trigger` takes the same request | EVT-001; the two RPC names: Out of scope: 0.3 |
| l.160 | name field is `event`, not `name` | Out of scope: 0.3 (wire) |
| l.162-168 | `version` optional, omitted means 1 | EVT-002 (reads back version 1) |
| l.164-166 | version selects schema; unregistered version on a governed name rejected | 0.12 (moved: header, schema enforcement) |
| l.167 | negative version returns `invalid_argument` | EVT-008 |
| l.171 | `metadata` optional, any key-value pairs | EVT-002 |
| l.171 | metadata delivered to handlers | 0.4 (moved: header, triggers) |
| l.173 | repeat key in same environment returns first `eventId`, stores no second event | EVT-030 |
| l.173 | ... and the first call's run IDs | 0.4 (moved: header, triggers and `runIds`) |
| l.173 | "in the same environment" | deferred to 0.9 |
| l.173 | `dataValue` carries a non-object payload | EVT-003 |
| l.175-177 | name under 255 bytes with `events:` prefix, so 248 bytes | Changed: 256 bytes, no prefix (EVT-011, EVT-012) |
| l.177-179 | no `*`/`>`, whitespace, control char, empty segment; each `400` | Whitespace, control char, empty segment: EVT-011. Wildcards: Not carried, no wildcard ban until a release uses patterns (decision 84) |
| l.179 | non-ASCII names allowed | EVT-012 |
| l.179-180 | same bound on NATS ingest | 0.17 (moved: header) |
| l.183-191 | response `eventId`; no `runIds` when nothing matches | EVT-001 |
| l.190-191 | `runIds` omitted rather than `[]` | Out of scope: 0.3 (wire) |
| `GET /events` table, `limit` | default 20, clamp 200, non-positive integer `400` | EVT-046; non-positive: excluded by type (`NonZeroU32`), a wire case for 0.3 |
| `limit` | a non-number `400` | deferred to 0.3 |
| `cursor` | forward keyset token | EVT-042, EVT-043 |
| `before`; "Errors" l.269 | backward token; mutex with `cursor` | deferred to 0.4 |
| `name`, `names`, `search`, `source`, `since`, `until` | filters, max 200 values | deferred to 0.4, deferred to 0.4 |
| l.237-238 | sorted newest first by `(timestamp, id)` | EVT-041; tie-break by ID: Not carried (header, decision 37) |
| l.238 | always scoped to caller's environment | deferred to 0.9; ACC-023 in `../../0.2-tenancy/behaviors/tenancy.feature` |
| l.258-261 | `has_next`/`has_prev` direction-aware; newest page `has_prev=false` | EVT-042; `has_prev`: deferred to 0.4 |
| l.262-264 | `approx_total` exact below 10,000, capped above | deferred to 0.4 |
| l.265-266 | timestamps RFC 3339 with milliseconds | deferred to 0.3 |
| l.269 | invalid `cursor`/`before` token `400` | EVT-045 |
| l.270-278 | bad `since`/`until`; >200 `names`/`source` | deferred to 0.4 |
| `GET /events/names` (l.279-344) | names with counts, scan cap 10,000, 200 names, `truncated` | deferred to 0.4 |
| `GET /events/{id}` (l.374) | `404` when not in caller's environment | EVT-040; cross-environment: 0.8 |
| `GET /events/{id}` fields | `id`, `name`, `data`, `metadata` | EVT-002 |
| fields | `emitted_by` | ACC-060 (0.2) |
| fields | `run_id` | deferred to 0.4 |
| fields | `processed` | 0.4 (moved: dispatch) |
| fields | `entity_id`, `entity_type` | 0.13 (moved: header) |
| fields | `source`, `timestamp`, `created_at` | Out of scope: 0.3 (wire, served-surface differ) |
| fields | `idempotency_key` reads back | EVT-002 (resolved) |
| "Pub/Sub" (l.1986-2028) | publish key rules | 0.14 (moved: header) |
| "Event Schema Registry" (l.2029) | schemas, upcast | 0.12 (moved: header) |

### `docs/reference/cli/commands/emit.md`

| Source | Claim | Maps to |
|---|---|---|
| l.6, l.82-86 | emit prints event ID and triggered runs | EVT-001; runs: 0.4 |
| l.12, l.88-99 | `--wait` blocks for runs | 0.4 (moved: header, TriggerSync) |
| l.27 | repeat key returns the original event | EVT-030 |
| l.27 | keys shared across the environment; never reuse for a different event | EVT-032 |
| l.29 | `--metadata` key=value pairs | EVT-002; flag parsing: Out of scope: 0.3 (CLI) |
| l.32 | `--version` default 1 | EVT-002 (server default); flag: Out of scope: 0.3 (CLI) |
| l.38-43 | one data source; stdin rules; defaults to `{}` | Out of scope: 0.3 (CLI-side) |
| l.43 | payload can be any JSON value | EVT-003 |

### `docs/reference/cli/commands/event.md`

| Source | Claim | Maps to |
|---|---|---|
| `events explain` (l.14-59) | routing eligibility, idempotency hit returns `duplicate` | Out of scope: 0.4 routing (not named in header; see Notes) |
| `event schema *` (l.61-371) | schema register/list/get/delete/history/check | 0.12 (moved: header) |
| `event upcast` (l.373) | upcast dry run | 0.12 (moved: header) |
| `event redact` (l.425-474) | replaces data with `$redacted` placeholder | Out of scope: 0.16 erasure (not named in header; see Notes) |

### `docs/reference/reserved-names.md`

| Source | Claim | Maps to |
|---|---|---|
| l.9 | a rejected write fails with an error, never silently | EVT-013, EVT-061 |
| Roots l.21 | `ironflow.` rejected | EVT-013 |
| l.21, l.78-80 | except `ironflow.invoke.<fn>` with `functions:invoke` | 0.4 (moved: header, invoke); permission: 0.8 |
| l.22 | `$` rejected | EVT-013 |
| l.21 | NATS ingest rejects it | 0.17 (moved: header) |
| l.23-37 | webhook, entity, KV, projection, topic, Rego, environment roots | 0.12 / 0.13 / 0.10 / 0.14 / 0.8 (other capabilities) |
| l.39 | event roots match exact case | EVT-012 (resolved) |
| l.41 | schema registration rejects owned names | 0.12 (moved: header) |
| "Errors" l.45 | event names answer `invalid_argument` | EVT-013 |
| Payload markers l.57, l.72 | `$enc`-only object reserved, no door rejects it in 1.0 | Not carried: reserved, unsupported (resolved) |
| l.58-60 | top-level `$redacted` rejected whatever its value | EVT-014 |
| l.60 | nested `$redacted` passes | EVT-015 |
| l.62 | marker check on `Trigger`, `Emit`, `TriggerBatch` | EVT-014; `TriggerSync`: 0.4; `PubSubService.Emit`: 0.14; `AppendEvent`: 0.13; webhook: 0.12; NATS: 0.17 |
| l.63-70 | invoke, PatchStep, worker outputs, `waitForEvent` payloads | Out of scope: caps 1, 2 (0.4-0.7) |
| l.82-86 | platform-only `ironflow.file.*`, `.bucket.*`, `.cron.<fn>`, `.function.invoked` rejected | EVT-013 |
| l.88 | every other `ironflow.` name rejected | EVT-013 |
| l.90 | webhook and entity append get no exceptions | 0.12, 0.13 |
| l.94-95 | triggers can match and clients can read owned events | 0.4; owned events only exist once platform writers ship |
| l.100-107 | checks only on user calls; stored old names stay | Not carried: no pre-1.0 upgrade paths; importer 0.18 |

### `docs/reference/configuration.md`

| Source | Claim | Maps to |
|---|---|---|
| "API Payload Limits", Max Event Data (l.902) | 512 KB measured as JSON; `invalid_argument`; no event written | EVT-017 |
| l.902 | covers `TriggerSync`, `PubSubService.Emit`, `AppendEvent`, invoke, A2A | 0.4 / 0.14 / 0.13 / 0.6 |
| l.902 | webhook bodies 1 MB | 0.12 |
| Unique keys and storable text (l.903) | data and metadata: no repeated key, NUL, invalid UTF-8, unpaired surrogate; `invalid_argument`; nothing written | EVT-016; invalid UTF-8: EVT-016 too, since data and metadata are bytes (D22; the rows arrive with #23) |
| l.903 | idempotency key: no NUL | EVT-016 |
| l.903 | idempotency key: no invalid UTF-8 | Out of scope: 0.3 (raw bytes reach only through a header door; `Operations` takes valid UTF-8). No 0.3 scenario yet: note for the 0.3 sweep |
| l.903 | NATS terminates, webhooks keep one value for a repeated key | 0.17, 0.12 |
| Max Batch Size (l.907) | 100 | EVT-021 |
| Wait Timeout (l.908) | 30 s default for `emit --wait` | 0.4 / 0.11 |
| `IRONFLOW_EVENT_RETENTION_DAYS` (l.576) | 0 keeps events forever; pruner | Out of scope: retention pruner, no release row (see Notes); EVT-033 holds while the event exists |
| `IRONFLOW_EVENT_SCHEMA_ENFORCEMENT` (l.688-751) | off/warn/reject | 0.12 (moved: header) |

### SDK references (`docs/reference/api/`, `docs/reference/js-sdk/`)

| Source | Claim | Maps to |
|---|---|---|
| `go-sdk.md` l.680-700 | `Emit` returns `EventID`, `RunIDs`; key, version and metadata options | EVT-001, EVT-030, EVT-009, EVT-002; options: SDK-side |
| `go-sdk.md` l.709 | `Emit` returns once the server stored the event | EVT-060 |
| `go-sdk.md` l.709 | ... and created the runs | 0.4 |
| `go-sdk.md` l.2416-2419 | `ListEvents`, `GetEvent`, `ListEventNames`, `TriggerBatch` methods | EVT-041, EVT-002, deferred to 0.4, EVT-020 |
| `go-sdk.md` l.2609-2611 | `AlreadyExists` = "reused idempotency key" | SDK-side; server source is `InvokeFunction` only (`engine/api.go:449`), 0.6. Not an emit conflict |
| `go-sdk.md` l.334, `python-sdk.md` l.207 | handler event carries `IdempotencyKey`, `Version`, `Source` | 0.4 (moved: triggers) |
| `go-sdk.md` l.598-605, `node.md` l.406, `core.md` l.604-629 | publish key: 255 bytes, no whitespace, retention | 0.14 (moved: header) |
| `core.md` l.632-660, `go-sdk.md` l.722-779 | `emitSync` / `EmitSync` | 0.4 (moved: header, TriggerSync) |
| `core.md` l.706-724 | `triggerBatch` items: event, data, version, key, metadata | EVT-020; per-item key: EVT-030 (ADR 0128: batch runs `Trigger` per item) |
| `core.md` l.725-733 | one result per event, in order; error present on a failed item; others still emit | EVT-020 |
| `core.md` l.730 | `eventId` empty when the item failed | EVT-020 (resolved) |
| `core.md` l.735-770 | `listEvents` options and result fields | EVT-041/042/046; filters, `before`, totals: deferred to 0.4/047/048 (0.4) |
| `core.md` l.753 | `since`/`until` `Date` cut to ms; stored timestamps finer | 0.4 (filters); SDK-side cut |
| `core.md` l.772-778 | `getEvent(id)` returns a stored event | EVT-002, EVT-040 |
| `core.md` l.780-795 | `listEventNames` | deferred to 0.4 |
| `node.md` l.281, `browser.md` l.1039 | `emittedBy` from the caller's credential | ACC-060 (0.2) |
| `node.md` l.569, `go-sdk.md` l.715 | emit from a function inside a step | SDK-side; deferred to 0.4 |
| `browser.md` l.2040-2128 (ADR 0053) | offline outbox, dedup relies on the engine key | SDK-side; engine part EVT-030 |
| `python-sdk.md` l.1440-1455 | writes sent exactly once; repeat with `idempotency_key` | SDK-side; engine part EVT-030 |
| `python-sdk.md` l.393, l.536-538 | retry only idempotent methods | SDK-side |
| `sdk-comparison.md`, `sdk-parity-matrix.md` | SDK feature parity | SDK-side |

### Explanation and how-to pages

| Source | Claim | Maps to |
|---|---|---|
| `explanation/outbox.md` l.10, l.21-47, l.65-99, l.208-219 | event and outbox row in one transaction; outbox worker, DLQ; `WaitForEvent` for read your writes; NATS seq unset 10-50 ms | Removed (decision 57) |
| `explanation/outbox.md` l.56 | acceptance confirms the Ironflow commit | EVT-060 |
| `explanation/outbox.md` l.51, l.56 | retry with a stable key per event | EVT-030 |
| `explanation/outbox.md` l.96 | `AppendEvent` sequence is 0 | 0.13 (moved: header) |
| `explanation/file-storage.md` l.80 | client emit of `ironflow.file.`/`ironflow.bucket.` rejected | EVT-013 |
| `explanation/pubsub.mdx` l.25-50 | `emit()` writes the `events:` namespace and triggers workflows | Changed: no namespace prefix, so nothing counts against the 256-byte limit but the name (EVT-011); triggering: 0.4 |
| `explanation/patterns.md` l.360-385 | read your own writes on projections | 0.13 |
| `explanation/stack-fit.md` l.98 | exactly-once only within a run; downstream needs keys | Out of scope: cap 1 (0.4) |
| `explanation/crash-recovery.md` l.88 | emit commits a run in `waiting_for_capacity` first | 0.4/0.5 |
| `explanation/ddd/why-ddd.md`, `ai-pitfalls.mdx`, `use-cases/*` | idempotency keys on external calls | Out of scope: application guidance, no engine claim |
| `how-to-guides/integration/other-languages.md` l.22-29, l.220 | CLI emit; `POST .../Emit` | 0.3 (CLI and door) |
| `how-to-guides/workflows/steps.mdx` l.569 | `step.publish` default key | 0.14 |
| `how-to-guides/workflows/debounce.md` l.151 | `TriggerSync` rejects debounced functions | 0.4 / 0.9 |
| `reference/mcp-tools.md` l.67, l.126, l.215-240 | `ironflow_emit_event`, `ironflow_list_events`; `since` re-returns the boundary event | 0.11 (MCP door); `since`: deferred to 0.4 |
| `reference/nats-ingestion.md` | ingest subject, rejections | 0.17 (moved: header) |

## 3. ADRs

| Source | Claim | Maps to |
|---|---|---|
| 0007 §Context, §9 | idempotency and event-plus-outbox in one funnel | EVT-030; outbox: Removed (decision 57) |
| 0007 §9 (024a/024b), Consequences | key unique per `(environment_id, idempotency_key)` | EVT-032, deferred to 0.9 |
| 0007 Consequences | `UNIQUE(event_id, function_id)` runs | 0.4 |
| 0068 Amendment | version 0 means unset, coerced to 1 | Changed: `Option` carries absence, so 0 is refused (EVT-008) and absent stores 1 (EVT-002) |
| 0068 Amendment | `< 0` refused `InvalidArgument` on every funnel | EVT-008 |
| 0068 Decision | schema enforcement on emit | 0.12 (moved: header) |
| 0128 Roots, "Three kinds" | owned event roots; platform-only and writable names | EVT-013; `ironflow.invoke.<fn>`: 0.4 |
| 0128 "User doors" | `TriggerBatch` reports a rejected owned name on that item; others emit | EVT-020 |
| 0128 "Webhooks get no exceptions" | | 0.12, 0.13 |
| 0128 "The slash rename" | old names stay in history | Not carried: no pre-1.0 upgrade paths |
| 0129 "Funnel placement" | per-name check after owned-name check, before idempotency lookup; retry of a denied name with a known key denied | EVT-036 (gate order); authorization: 0.8 (moved: header) |
| 0129 Provenance | every new event has `emitted_by` | ACC-060 (0.2) |
| 0130 l.31 | `TriggerBatch` item error text sanitized | Out of scope: 0.3 (door error text) |
| 0132 Context l.14 | `UNIQUE (environment_id, idempotency_key)` dedups `Emit` for good | EVT-033 |
| 0132 Decision | publish key store, run retention | 0.14 (moved: header) |
| 0133 l.49-54 | one-member `$enc` object reserved, no door check in 1.0 | Not carried: reserved, unsupported (resolved) |
| 0133 l.57-59 | top-level `$redacted` rejected | EVT-014 |
| 0153 | explain routing through a read-only matcher | Out of scope: 0.4 routing (see Notes) |
| 0167 | EVENTS stream limits, broker backpressure | Out of scope: 0.17 (NATS); internal |
| 0175 Decision | duplicate key at any depth refused | EVT-016 |
| 0175 Decision | keys compare after unescaping | EVT-016 (resolved) |
| 0175 Decision | NUL in a key or string; a top-level string counts | EVT-016 (resolved) |
| 0175 Decision | invalid UTF-8; unpaired high or lone low surrogate | Surrogates: EVT-016; invalid UTF-8: EVT-016 (D22; the rows arrive with #23) |
| 0175 Decision | metadata gets the same check | EVT-016 |
| 0175 Decision | idempotency key: NUL refused, no key format added | NUL: EVT-016. Changed: 1–256 bytes, `""` refused (EVT-035, EVT-037) |
| 0175 Decision | idempotency key: invalid UTF-8 | Out of scope: 0.3 (header bytes) |
| 0175 Decision | funnel refuses before its idempotency lookup | EVT-036 |
| 0175 Decision | stored rows not re-validated | Not carried: no pre-1.0 upgrade paths |
| 0175 "Text outside event data" | outputs, errors, registrations | Out of scope: caps 1, 2, 4, 5 |

## Gaps

**Resolved 2026-11-17.** GAP-1 is a row in EVT-012; GAP-2 is not carried, because `$enc` is reserved and data using it is unsupported; GAP-3 is a step in EVT-020; GAP-4 is a step in EVT-002; GAP-5 is two rows in EVT-016. The three surfaces under Notes are in the feature's header (0.4, 0.16, 0.16), and the invalid-UTF-8 key is a 0.3 line. The proposals below are kept as the record.

All five are 0.1 scope. The IDs proposed below (EVT-024, EVT-025, EVT-037) were never assigned to these proposals; they now hold other scenarios.

**GAP-1: owned event roots are case-sensitive.** `reserved-names.md` l.39: "Event roots ... match exact case, because event names ... are case-sensitive." EVT-013 shows only that the exact-case roots are refused. A Rust check that folds case would pass EVT-013 and break this.

```gherkin
  @EVT-024 @release-0.1
  # go: docs/reference/reserved-names.md "Roots" (event roots match exact case)
  # go: internal/reserved/reserved_test.go
  Scenario Outline: Owned name roots match exact case
    When the client emits event <name>
    Then the emit succeeds

    Examples:
      | name                |
      | "Ironflow.run.done" |
      | "IRONFLOW.tick"     |
```

**GAP-2: an `$enc` envelope is accepted.** `reserved-names.md` l.57 and l.72, ADR 0133 l.54: `$enc` is reserved by contract, and no door rejects it in 1.0. No scenario pins that it passes. A Rust gate that refuses every `$`-prefixed key would break it.

```gherkin
  @EVT-025 @release-0.1
  # go: docs/reference/reserved-names.md "Payload markers" ($enc: no door rejects it in 1.0)
  # go: docs/adr/0133-client-side-field-level-payload-encryption.md
  Scenario: An encryption envelope in the payload is accepted and reads back unchanged
    When the client emits event "order.placed" with data {"card": {"$enc": {"v": 1, "ct": "x"}}}
    And the client reads that event by ID
    Then its data is {"card": {"$enc": {"v": 1, "ct": "x"}}}
```

**GAP-3: a failed batch item has no event ID.** `js-sdk/core.md` l.730: "`eventId` ... Empty when the item failed." EVT-020 checks the error and the other items, but not that the failed item has no event.

Proposed: add one step to EVT-020 rather than a new ID.

```gherkin
    And the second item has no event ID
```
(provenance: `# go: docs/reference/js-sdk/core.md "triggerBatch"`)

**GAP-4: an event reads back with its idempotency key.** `rest-api.md` `GET /events/{id}` and `GET /events` list `idempotency_key` as a response field. EVT-002 reads back name, data and metadata only. This could instead be judged 0.3 wire, which the served-surface differ pins. Decide which before handoff.

```gherkin
  @EVT-037 @release-0.1
  # go: docs/reference/api/rest-api.md "GET /events/{id}" (idempotency_key)
  # go: internal/server/coverage_bump_test.go TestHandleGetEvent_AllPaths
  Scenario: An event reads back with its idempotency key
    When the client emits event "order.placed" with idempotency key "order-1"
    And the client reads that event by ID
    Then its idempotency key is "order-1"
```

**GAP-5: duplicate keys compare after unescaping, and a NUL in a top-level string counts.** ADR 0175 Decision. EVT-016's rows do not separate these cases, and both are easy to miss: serde_json keeps the last of two equal keys, and the escaped spelling hides the duplicate.

Proposed: add two rows to the EVT-016 Examples.

```gherkin
      | data            | keys equal after unescaping, "a" and "a" |
      | data value      | a top-level string holding a NUL              |
```
(provenance: `# go: docs/adr/0175-require-unique-keys-in-event-data.md`)

## Conflicts

Deliberate changes, each a behavior Ironflow X states on purpose:

- Name limit 256 bytes with no namespace prefix, where the docs give 248 (EVT-011, EVT-012).
- Owned names are `ironflowx`, `ironflowx.…`, `$system` and `$system.…`, exact case (EVT-012, EVT-013), per decision 84.
- Version 0 is refused instead of coerced to 1 (EVT-008); an empty idempotency key is refused instead of meaning no key (EVT-037).
- An empty batch answers with no results, as today (EVT-023); a key repeated inside one batch dedups (EVT-022); each item's metadata is capped at 64 KiB (EVT-038), and a batch's data plus metadata at 4 MiB (EVT-025), both new.
- Data and metadata are stored and returned as the bytes sent, and the size limit counts those bytes, not compact JSON (EVT-017, EVT-039). Nesting is capped at 127, `serde_json`'s limit.

No other docs claim or conformance scenario contradicts a scenario. Three near-misses, none blocking:

- `rest-api.md` l.237 sorts by `(timestamp, id)`. EVT-041 uses commit order, and the header drops the ID tie-break (decision 37). The two orders also differ if the wall clock steps back. The public docs need rewording when Rust ships, not the spec.
- `emit.md` l.43: data defaults to `{}`. EVT-004 stores `null` when the data is absent. The CLI fills in `{}` before it sends, so these do not conflict.
- `go-sdk.md` l.2611: `AlreadyExists` means "reused idempotency key". On the server that comes only from `InvokeFunction` (`internal/engine/api.go:449`), not from emit. EVT-030 stands.

## Notes for the plan (not 0.1 gaps)

- Three event-capability surfaces have no release line in the header: `ExplainEventMatch` / `ironflow events explain` (ADR 0153), `RedactEvent` / `ironflow event redact`, and the `IRONFLOW_EVENT_RETENTION_DAYS` pruner. Add each to the header's "Later in this capability" line, or to a release row.
- Idempotency key with invalid UTF-8 (configuration.md l.903, ADR 0175): carry it into the 0.3 sweep as a door scenario.
- The JSON door (0.3) embeds event data as a JSON value, with no base64, and passes that value's raw bytes through without re-serialising it, or "stored as sent" breaks (EVT-039, D22).
