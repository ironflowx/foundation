//! Event rules through `decide`, `fold` and the queries, over an in-memory store.

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::convert::Infallible;
use std::ops::Bound::{Excluded, Unbounded};

use std::num::NonZeroU32;

use ironflowx_core::{Emitter, ErrorKind, EventId, IncarnationToken, SequencedTime, Timestamp};
use ironflowx_events::{
    Decision, Emit, EngineEmit, EngineEvent, Error, Event, EventPage, EventTables, EventsView,
    ItemOutcome, NewEvent, PageBudget, Record, Submission, check_emit, decide, engine_event, fold,
    get_event, list_events, sequence,
};

/// The read tables, in memory: the one seam.
#[derive(Default, Debug, PartialEq)]
struct Store {
    events: BTreeMap<u64, Event>,
}

impl EventsView for Store {
    type Error = Infallible;
    fn event_by_key(&self, key: &str) -> Result<Option<EventId>, Infallible> {
        Ok(self
            .events
            .values()
            .find(|e| e.idempotency_key.as_deref() == Some(key))
            .map(|e| e.id))
    }
    fn last_sequence(&self) -> Result<u64, Infallible> {
        Ok(self.events.keys().next_back().copied().unwrap_or(0))
    }
    fn event(&self, id: EventId) -> Result<Option<Event>, Infallible> {
        Ok(self.events.values().find(|e| e.id == id).cloned())
    }
    fn events_before(
        &self,
        before: Option<u64>,
        budget: &mut PageBudget,
    ) -> Result<Vec<Event>, Infallible> {
        Ok(self
            .events
            .range((Unbounded, before.map_or(Unbounded, Excluded)))
            .rev()
            .map(|(_, e)| e)
            .take_while(|e| budget.take(size(e)))
            .cloned()
            .collect())
    }
}

/// The bytes of an event's data and metadata.
fn size(e: &Event) -> usize {
    e.data.len() + e.metadata.as_ref().map_or(0, Vec::len)
}

/// The sequence of a row the store already holds.
#[derive(Debug, PartialEq)]
struct DuplicateRow(u64);

impl EventTables for Store {
    type Error = DuplicateRow;
    /// Refuses a second row for a sequence, as a unique-keyed table does.
    fn insert(&mut self, event: Event) -> Result<(), DuplicateRow> {
        match self.events.entry(event.sequence) {
            Entry::Occupied(_) => Err(DuplicateRow(event.sequence)),
            Entry::Vacant(row) => {
                row.insert(event);
                Ok(())
            }
        }
    }
}

const NOW: Timestamp = Timestamp::from_millis(1_700_000_000_000);

/// The event-clock reading every test decides at.
// D17: only the ledger stamps event time; this helper stands in for it, and only it may.
#[expect(clippy::disallowed_methods)]
fn now() -> SequencedTime {
    SequencedTime::from_event_clock(NOW)
}

/// The emitter every test folds with.
fn by() -> Emitter {
    "embedder:rules".parse().expect("an emitter")
}

/// Distinct valid UUIDv7 event IDs, 1, 2, 3, ...
fn ids() -> impl Iterator<Item = EventId> {
    (1u32..).map(|n| {
        format!("019a8f3c-7e21-7b4a-9c3d-{n:012x}")
            .parse()
            .expect("valid UUIDv7")
    })
}

fn id(n: u32) -> EventId {
    ids().nth(n as usize - 1).expect("id")
}

fn named(name: &str) -> Emit {
    Emit {
        name: name.into(),
        ..Emit::default()
    }
}

/// A ledger in miniature: decide, then fold what it returns.
struct Env {
    store: Store,
    fresh: Box<dyn Iterator<Item = EventId>>,
}

impl Env {
    fn new() -> Self {
        Self {
            store: Store::default(),
            fresh: Box::new(ids()),
        }
    }

    fn submit(&mut self, sub: Submission) -> Result<Vec<ItemOutcome>, Error<Infallible>> {
        let Decision { outcome, record } = decide(&self.store, &sub, now(), &mut self.fresh)?;
        if let Some(record) = record {
            fold(&mut self.store, &record, &by()).expect("in memory");
        }
        Ok(outcome)
    }

    fn emit(&mut self, emit: Emit) -> Result<ItemOutcome, Error<Infallible>> {
        let mut outcome = self.submit(Submission::Emit(emit))?;
        assert_eq!(outcome.len(), 1);
        Ok(outcome.remove(0))
    }

    fn stored(&self) -> Vec<String> {
        self.store.events.values().map(|e| e.name.clone()).collect()
    }
}

/// The emit is refused as invalid and nothing is stored.
fn assert_refused(emit: Emit) -> String {
    let mut env = Env::new();
    let message = match env.emit(emit) {
        Err(Error::Refused(r)) => {
            assert_eq!(r.kind(), ErrorKind::Invalid);
            assert_eq!(r.hint(), None, "no event rule hints yet");
            r.message().to_owned()
        }
        other => panic!("expected a refusal, got {other:?}"),
    };
    assert!(env.stored().is_empty());
    message
}

/// The emit is accepted; returns the event as read back by ID.
fn assert_accepted(emit: Emit) -> Event {
    let mut env = Env::new();
    match env.emit(emit) {
        Ok(ItemOutcome::New(id)) => get_event(&env.store, &id.to_string()).expect("readable"),
        other => panic!("expected a new event, got {other:?}"),
    }
}

// ---- Name ----

#[test]
fn an_event_with_a_valid_name_is_stored_and_reads_back() {
    let event = assert_accepted(named("order.placed"));
    assert_eq!(
        event,
        Event {
            sequence: 1,
            id: id(1),
            name: "order.placed".into(),
            data: "null".into(),
            metadata: None,
            version: 1,
            idempotency_key: None,
            time: NOW,
            emitted_by: by(),
        }
    );
}

#[test]
fn an_emit_with_no_name_is_refused_as_required() {
    assert_eq!(assert_refused(named("")), "event name is required");
}

#[test]
fn a_name_is_1_to_256_bytes_counted_in_utf8() {
    assert_accepted(named("a"));
    assert_accepted(named(&"a".repeat(256)));
    assert_accepted(named(&"é".repeat(128)));
    assert_refused(named(&"a".repeat(257)));
    assert_refused(named(&"é".repeat(129)));
}

#[test]
fn a_name_with_an_empty_segment_whitespace_or_a_control_character_is_refused() {
    for name in [
        "a..b",
        ".a",
        "a.",
        "order placed",
        "order\u{a0}placed",
        "order\tplaced",
        "order\u{0}placed",
        "order\u{7f}placed",
    ] {
        assert_refused(named(name));
    }
    assert_accepted(named("订单.placed"));
}

#[test]
fn owned_names_are_refused_in_exact_case_only() {
    for name in ["ironflowx", "ironflowx.run.done", "$system", "$system.tick"] {
        assert_refused(named(name));
    }
    for name in [
        "Ironflowx.run.done",
        "ironflowxy.done",
        "$System.tick",
        "$custom.tick",
        "$systems",
    ] {
        assert_accepted(named(name));
    }
}

// ---- Data ----

fn with_data(data: &str) -> Emit {
    Emit {
        data: Some(data.into()),
        ..named("order.placed")
    }
}

fn nested_arrays(depth: usize) -> String {
    format!("{}{}", "[".repeat(depth), "]".repeat(depth))
}

/// `{"<key>": "aaa…"}` of exactly `len` bytes.
fn blob(key: &str, len: usize) -> String {
    let pad = len - format!(r#"{{"{key}": ""}}"#).len();
    format!(r#"{{"{key}": "{}"}}"#, "a".repeat(pad))
}

#[test]
fn any_json_value_is_stored_and_read_back_as_the_bytes_sent() {
    for data in [
        "false",
        r#"["payload"]"#,
        "null",
        r#""text""#,
        "42",
        r#"{"k": "😀"}"#,
        r#"{"k":"😀"}"#,
        r#"{"k": "\ud83d\ude00"}"#,
        r#"{"n": 1.10, "big": 123456789012345678901234567890}"#,
        "1.7976931348623157e308",
        r#"{  "spaced" :  true  }"#,
        &nested_arrays(127),
    ] {
        assert_eq!(assert_accepted(with_data(data)).data, data.as_bytes());
    }
}

#[test]
fn data_is_at_most_524288_bytes_as_sent() {
    assert_accepted(with_data(&blob("blob", 524_288)));
    assert_eq!(
        assert_refused(with_data(&blob("blob", 524_289))),
        "event data exceeds maximum size"
    );
    let padded = format!("{} ", blob("blob", 524_288));
    assert_refused(with_data(&padded));
}

#[test]
fn malformed_data_is_refused() {
    for data in [
        r#"{"a": 1, "a": 2}"#,
        r#"{"a": 1, "\u0061": 2}"#,
        r#"{"x": {"a": 1, "a": 2}}"#,
        r#"[{"a": 1, "a": 2}]"#,
        r#""x\u0000y""#,
        r#"{"k": "x\u0000y"}"#,
        r#"{"k\u0000": 1}"#,
        r#"{"k": "\ud800"}"#,
        r#"{"k": "\udc00"}"#,
        r#"["ok", "\ud800"]"#,
        r#"{"k\ud800": 1}"#,
        r#"{"k": 1"#,
        "",
        "1 2",
        "1e400",
        "-1e400",
        // serde_json's fast float parser rounds this to f64::MAX; correctly rounded it is inf.
        "1.79769313486231581e308",
        &"9".repeat(400),
        &nested_arrays(128),
        &format!("{}{}", r#"{"a":"#.repeat(128), "}".repeat(128)),
    ] {
        assert_refused(with_data(data));
    }
}

#[test]
fn a_top_level_redacted_key_is_refused_and_a_nested_one_accepted() {
    for data in [
        r#"{"$redacted": true}"#,
        r#"{"$redacted": false}"#,
        r#"{"a": 1, "$redacted": null}"#,
        r#"{"$redacted": 1}"#,
        r#"{"\u0024redacted": 1}"#,
    ] {
        assert_refused(with_data(data));
    }
    assert_accepted(with_data(r#"{"inner": {"$redacted": true}}"#));
    assert_accepted(with_data(r#"[{"$redacted": true}]"#));
}

#[test]
fn invalid_utf8_in_data_or_metadata_is_refused() {
    for bytes in [
        &b"\"\xff\""[..],
        b"{\"k\": \"\xff\"}",
        b"{\"\xff\": 1}",
        // A two-byte sequence cut short, an overlong "/", and a surrogate encoded as bytes.
        b"\"\xc3\"",
        b"\"\xc0\xaf\"",
        b"\"\xed\xa0\x80\"",
        // After an escape serde_json validates through its scratch buffer, another path.
        b"\"\\n\xff\"",
        b"\xff",
        b"{}\xff",
    ] {
        assert_refused(Emit {
            data: Some(bytes.to_vec()),
            ..named("order.placed")
        });
        assert_refused(Emit {
            metadata: Some([b"{\"t\": ", bytes, b"}"].concat()),
            ..named("order.placed")
        });
    }
    // The same shapes with valid UTF-8 are accepted, so the bytes above are what is refused.
    assert_accepted(with_data("\"\u{e9}\""));
    assert_accepted(with_metadata("{\"t\": \"\u{e9}\"}"));
}

#[test]
fn serde_jsons_number_key_is_an_ordinary_key() {
    // With `arbitrary_precision` on, numbers print as written, and such a build takes an object
    // whose first key is this one for a number (the shortcut in json.rs), so it is not probed.
    let printed = serde_json::from_str::<serde_json::Value>("0.10")
        .unwrap()
        .to_string();
    if printed == "0.10" {
        return;
    }
    for data in [
        r#"{"$serde_json::private::Number": "1", "a": 2}"#,
        r#"{"$serde_json::private::Number": "x"}"#,
        r#"{"$serde_json::private::Number": {"a": 1}}"#,
    ] {
        assert_accepted(with_data(data));
        assert_accepted(with_metadata(data));
    }
    // It is still held to every rule.
    assert_refused(with_data(
        r#"{"$serde_json::private::Number": "1", "$serde_json::private::Number": "2"}"#,
    ));
    assert_refused(with_data(r#"{"$serde_json::private::Number": "x\u0000"}"#));
}

// ---- Metadata ----

fn with_metadata(metadata: &str) -> Emit {
    Emit {
        metadata: Some(metadata.into()),
        ..named("order.placed")
    }
}

#[test]
fn metadata_is_absent_or_a_json_object_of_at_most_65536_bytes() {
    assert_eq!(assert_accepted(named("order.placed")).metadata, None);
    let at_limit = blob("t", 65_536);
    assert_eq!(
        assert_accepted(with_metadata(&at_limit)).metadata,
        Some(at_limit.into_bytes())
    );
    assert_accepted(with_metadata(r#"{"$redacted": 1}"#));
    assert_accepted(with_metadata(" \n{\"n\": 1.25}"));
    for metadata in [
        &blob("t", 65_537),
        r#"["t"]"#,
        "null",
        r#""t""#,
        "1.25",
        "1e400",
        r#"{"n": 1e400}"#,
        r#"{"t": 1, "t": 2}"#,
        r#"{"t": "x\u0000"}"#,
        r#"{"t": "\udc00"}"#,
        r#"{"t": 1"#,
        &format!("{}{}", r#"{"a":"#.repeat(128), "}".repeat(128)),
    ] {
        assert_refused(with_metadata(metadata));
    }
}

// ---- Version ----

fn with_version(version: i64) -> Emit {
    Emit {
        version: Some(version),
        ..named("order.placed")
    }
}

#[test]
fn version_is_1_when_absent_and_otherwise_1_to_2147483647_as_sent() {
    assert_eq!(assert_accepted(named("order.placed")).version, 1);
    for version in [1, 3, 2_147_483_647] {
        assert_eq!(assert_accepted(with_version(version)).version, version);
    }
    for version in [-1, 0, 2_147_483_648, i64::MIN, i64::MAX] {
        assert_refused(with_version(version));
    }
}

// ---- Idempotency key ----

fn with_key(name: &str, key: &str) -> Emit {
    Emit {
        idempotency_key: Some(key.into()),
        ..named(name)
    }
}

#[test]
fn an_idempotency_key_is_1_to_256_bytes_with_no_nul() {
    for key in ["order-1", &"a".repeat(256), r"literal \\u0000", "订单"] {
        assert_eq!(
            assert_accepted(with_key("order.placed", key)).idempotency_key,
            Some(key.to_owned())
        );
    }
    for key in ["", &"a".repeat(257), "k\0"] {
        assert_refused(with_key("order.placed", key));
    }
}

#[test]
fn a_repeated_key_returns_the_first_event_across_names_and_stores_nothing() {
    let mut env = Env::new();
    let first = env.emit(with_key("order.placed", "k-1")).expect("accepted");
    assert_eq!(first, ItemOutcome::New(id(1)));
    for name in ["order.placed", "order.cancelled"] {
        let sub = Submission::Emit(with_key(name, "k-1"));
        let decision = decide(&env.store, &sub, now(), &mut ids()).expect("accepted");
        assert_eq!(
            decision,
            Decision {
                outcome: vec![ItemOutcome::Duplicate(id(1))],
                record: None,
            }
        );
    }
    assert_eq!(env.stored(), ["order.placed"]);
}

#[test]
fn without_a_key_every_emit_is_a_new_event() {
    let mut env = Env::new();
    assert_eq!(env.emit(named("a")), Ok(ItemOutcome::New(id(1))));
    assert_eq!(env.emit(named("a")), Ok(ItemOutcome::New(id(2))));
    assert_eq!(env.stored(), ["a", "a"]);
}

#[test]
fn every_check_runs_before_the_idempotency_lookup() {
    let mut env = Env::new();
    env.emit(with_key("order.placed", "order-1"))
        .expect("accepted");
    for emit in [
        with_key("order placed", "order-1"),
        Emit {
            data: Some(r#"{"a": 1, "a": 2}"#.into()),
            ..with_key("order.placed", "order-1")
        },
        Emit {
            version: Some(-1),
            ..with_key("order.placed", "order-1")
        },
    ] {
        assert!(matches!(env.emit(emit), Err(Error::Refused(_))));
    }
    assert_eq!(env.stored(), ["order.placed"]);
}

// ---- Batch ----

fn refusal_of(result: Result<Vec<ItemOutcome>, Error<Infallible>>) -> String {
    match result {
        Err(Error::Refused(r)) => {
            assert_eq!(r.kind(), ErrorKind::Invalid);
            r.message().to_owned()
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[test]
fn a_batch_of_more_than_100_items_is_refused_whole() {
    let mut env = Env::new();
    let items = vec![named("a"); 101];
    assert_eq!(
        refusal_of(env.submit(Submission::Batch(items))),
        "batch size 101 exceeds maximum of 100"
    );
    assert!(env.stored().is_empty());
    let outcome = env.submit(Submission::Batch(vec![named("a"); 100]));
    assert_eq!(outcome.expect("accepted").len(), 100);
}

#[test]
fn an_empty_batch_answers_with_no_results() {
    let decision = decide(
        &Store::default(),
        &Submission::Batch(vec![]),
        now(),
        &mut ids(),
    );
    assert_eq!(
        decision,
        Ok(Decision {
            outcome: vec![],
            record: None,
        })
    );
}

#[test]
fn a_bad_item_is_refused_on_its_own_and_the_others_are_stored_in_input_order() {
    let mut env = Env::new();
    let outcome = env
        .submit(Submission::Batch(vec![
            named("a.ok"),
            named(""),
            named("b.ok"),
        ]))
        .expect("batch accepted");
    assert_eq!(outcome[0], ItemOutcome::New(id(1)));
    assert!(
        matches!(&outcome[1], ItemOutcome::Refused(r) if r.message() == "event name is required")
    );
    assert_eq!(outcome[2], ItemOutcome::New(id(2)));
    assert_eq!(env.stored(), ["a.ok", "b.ok"]);
    let sequences: Vec<_> = env.store.events.values().map(|e| e.sequence).collect();
    assert_eq!(sequences, [1, 2]);
}

#[test]
fn a_key_repeated_inside_one_batch_returns_the_earlier_items_event() {
    let mut env = Env::new();
    let outcome = env
        .submit(Submission::Batch(vec![
            with_key("a.ok", "k-1"),
            with_key("b.ok", "k-1"),
        ]))
        .expect("batch accepted");
    assert_eq!(
        outcome,
        [ItemOutcome::New(id(1)), ItemOutcome::Duplicate(id(1))]
    );
    assert_eq!(env.stored(), ["a.ok"]);
}

/// A view over a store that fails on `last_sequence` and on looking up `bad_key`.
struct Failing<'a> {
    store: &'a Store,
    bad_key: &'a str,
}

impl EventsView for Failing<'_> {
    type Error = &'static str;
    fn event_by_key(&self, key: &str) -> Result<Option<EventId>, &'static str> {
        if key == self.bad_key {
            return Err("lookup failed");
        }
        Ok(self.store.event_by_key(key).expect("in memory"))
    }
    fn last_sequence(&self) -> Result<u64, &'static str> {
        Err("last sequence failed")
    }
    fn event(&self, _: EventId) -> Result<Option<Event>, &'static str> {
        unreachable!()
    }
    fn events_before(
        &self,
        _: Option<u64>,
        _: &mut PageBudget,
    ) -> Result<Vec<Event>, &'static str> {
        unreachable!()
    }
}

#[test]
fn the_last_sequence_is_read_only_when_an_event_is_new() {
    let mut env = Env::new();
    env.emit(with_key("seen", "k-0")).expect("accepted");
    let view = Failing {
        store: &env.store,
        bad_key: "bad",
    };
    for (items, outcome) in [
        (vec![], vec![]),
        (vec![named("")], vec!["refused"]),
        (
            vec![with_key("dup", "k-0"), named("")],
            vec!["duplicate", "refused"],
        ),
    ] {
        let decision =
            decide(&view, &Submission::Batch(items), now(), &mut ids()).expect("decided");
        let kinds: Vec<_> = decision
            .outcome
            .iter()
            .map(|o| match o {
                ItemOutcome::New(_) => "new",
                ItemOutcome::Duplicate(_) => "duplicate",
                ItemOutcome::Refused(_) => "refused",
            })
            .collect();
        assert_eq!(kinds, outcome);
        assert_eq!(decision.record, None);
    }
}

#[test]
fn a_failed_lookup_draws_no_id() {
    let view = Failing {
        store: &Store::default(),
        bad_key: "bad",
    };
    let sub = Submission::Batch(vec![named("new"), with_key("later", "bad")]);
    let mut fresh = ids();
    assert_eq!(
        decide(&view, &sub, now(), &mut fresh),
        Err(Error::Store("lookup failed"))
    );
    assert_eq!(fresh.next(), Some(id(1)));
}

#[test]
fn a_refused_item_does_not_claim_its_key() {
    let mut env = Env::new();
    let outcome = env
        .submit(Submission::Batch(vec![
            with_key("", "k"),
            with_key("a.ok", "k"),
            with_key("b.ok", "k"),
        ]))
        .expect("batch accepted");
    assert!(matches!(outcome[0], ItemOutcome::Refused(_)));
    assert_eq!(
        outcome[1..],
        [ItemOutcome::New(id(1)), ItemOutcome::Duplicate(id(1))]
    );
    assert_eq!(env.stored(), ["a.ok"]);
}

#[test]
fn ids_are_drawn_only_for_new_events() {
    let mut env = Env::new();
    env.emit(with_key("seen", "k-0")).expect("accepted");
    let sub = Submission::Batch(vec![
        with_key("dup", "k-0"),
        named(""),
        named("new.1"),
        with_key("new.2", "k-2"),
        with_key("dup.2", "k-2"),
    ]);
    let mut fresh = ids().skip(10);
    let decision = decide(&env.store, &sub, now(), &mut fresh).expect("accepted");
    assert_eq!(fresh.next(), Some(id(13)));
    let record = decision.record.expect("new events");
    assert_eq!(record.time, NOW);
    let new: Vec<_> = record
        .events
        .iter()
        .map(|e| (e.sequence, e.id, e.name.as_str()))
        .collect();
    assert_eq!(new, [(2, id(11), "new.1"), (3, id(12), "new.2")]);
}

#[test]
fn a_batch_whose_accepted_data_and_metadata_total_over_4_mib_is_refused_whole() {
    let mut env = Env::new();
    let big = with_data(&blob("blob", 524_288));
    assert_eq!(
        refusal_of(env.submit(Submission::Batch(vec![big.clone(); 9]))),
        "batch data and metadata exceed 4 MiB"
    );
    assert!(env.stored().is_empty());

    // 8 × 524,288 is exactly 4 MiB; a refused item's bytes do not count.
    let mut items = vec![big.clone(); 8];
    items.push(Emit {
        name: String::new(),
        ..big.clone()
    });
    assert_eq!(
        env.submit(Submission::Batch(items))
            .expect("accepted")
            .len(),
        9
    );

    // The byte check runs before the lookup: items whose key is stored still count.
    let mut env = Env::new();
    env.emit(with_key("seen", "k")).expect("accepted");
    let dup = Emit {
        idempotency_key: Some("k".into()),
        ..big.clone()
    };
    refusal_of(env.submit(Submission::Batch(vec![dup; 9])));
    assert_eq!(env.stored(), ["seen"]);

    // Metadata counts too.
    let mut items = vec![big; 8];
    items.push(with_metadata("{}"));
    refusal_of(env.submit(Submission::Batch(items)));
}

// ---- Fold ----

#[test]
fn replaying_the_same_records_into_fresh_tables_gives_the_same_rows() {
    let mut env = Env::new();
    let mut records = vec![];
    for sub in [
        Submission::Emit(with_key("a", "k")),
        Submission::Batch(vec![named("b"), with_data("[1]")]),
    ] {
        let decision = decide(&env.store, &sub, now(), &mut env.fresh).expect("accepted");
        let record = decision.record.expect("new events");
        fold(&mut env.store, &record, &by()).expect("in memory");
        records.push(record);
    }
    let mut rebuilt = Store::default();
    for record in &records {
        fold(&mut rebuilt, record, &by()).expect("in memory");
    }
    assert_eq!(rebuilt, env.store);
}

#[test]
fn folding_the_same_record_twice_leaves_rows_unchanged_when_the_caller_skips_applied_positions() {
    let mut env = Env::new();
    let sub = Submission::Batch(vec![with_key("a", "k"), named("b")]);
    let record = decide(&env.store, &sub, now(), &mut env.fresh)
        .expect("accepted")
        .record
        .expect("new events");
    // The log holds the record at position 0; the ledger skips applied positions (Spec 4).
    let log = [record];
    let mut applied = 0;
    for _ in 0..2 {
        for record in &log[applied..] {
            fold(&mut env.store, record, &by()).expect("not yet applied");
            applied += 1;
        }
    }
    assert_eq!(env.stored(), ["a", "b"]);
    // Unskipped, the unique-keyed read store refuses the record and keeps its rows.
    let once = env.store.events.clone();
    assert_eq!(fold(&mut env.store, &log[0], &by()), Err(DuplicateRow(1)));
    assert_eq!(env.store.events, once);
}

#[test]
fn every_event_of_a_record_carries_its_one_emitter() {
    let mut env = Env::new();
    let sub = Submission::Batch(vec![named("a"), named("b"), named("c")]);
    let record = decide(&env.store, &sub, now(), &mut env.fresh)
        .expect("accepted")
        .record
        .expect("new events");
    let key: Emitter = "key:019a9f3e-7c2b-7d41-8a3e-5f6b7c8d9e0f".parse().unwrap();
    fold(&mut env.store, &record, &key).expect("in memory");
    assert!(env.store.events.values().all(|e| e.emitted_by == key));
    assert_eq!(env.store.events.len(), 3);
}

// ---- Reading ----

fn token(n: u8) -> IncarnationToken {
    format!("{n:032x}").parse().expect("token")
}

/// An environment holding `n` events named "e.1" ... "e.<n>", emitted in that order.
fn with_events(n: usize) -> Env {
    let mut env = Env::new();
    for i in 1..=n {
        env.emit(named(&format!("e.{i}"))).expect("accepted");
    }
    env
}

fn list(env: &Env, limit: Option<u32>, cursor: Option<&str>) -> EventPage {
    list_events(
        &env.store,
        token(1),
        limit.and_then(NonZeroU32::new),
        cursor,
    )
    .expect("listed")
}

fn names(page: &EventPage) -> Vec<&str> {
    page.events.iter().map(|e| e.name.as_str()).collect()
}

#[test]
fn an_unknown_or_unparseable_id_is_not_found() {
    let env = with_events(1);
    for id in ["019a8f3c-7e21-7b4a-9c3d-5e6f7a8b9c0d", "not-a-uuid", ""] {
        match get_event(&env.store, id) {
            Err(Error::Refused(r)) => {
                assert_eq!(r.kind(), ErrorKind::NotFound);
                assert_eq!(r.message(), "event not found");
            }
            other => panic!("expected not found, got {other:?}"),
        }
    }
}

#[test]
fn events_list_newest_first_by_event_sequence() {
    let env = with_events(3);
    assert_eq!(names(&list(&env, None, None)), ["e.3", "e.2", "e.1"]);
}

#[test]
fn the_page_limit_defaults_to_20_and_clamps_at_200() {
    let env = with_events(250);
    assert_eq!(list(&env, None, None).events.len(), 20);
    assert_eq!(list(&env, Some(500), None).events.len(), 200);
}

#[test]
fn events_page_by_cursor_and_the_last_page_has_none() {
    let env = with_events(5);
    let first = list(&env, Some(2), None);
    assert_eq!(names(&first), ["e.5", "e.4"]);
    let second = list(&env, Some(3), first.next.as_deref());
    assert_eq!(names(&second), ["e.3", "e.2", "e.1"]);
    assert_eq!(second.next, None);

    let full = list(&env, Some(5), None);
    assert_eq!(full.events.len(), 5);
    assert_eq!(full.next, None);

    let empty = list(&Env::new(), None, None);
    assert!(empty.events.is_empty());
    assert_eq!(empty.next, None);
}

/// An environment holding one event per (data, metadata) size, oldest first, named "e.1", ...
/// Folded, not decided, so an event may be larger than one a client can emit.
fn with_sizes(sizes: &[(usize, Option<usize>)]) -> Env {
    let mut env = Env::new();
    let events = (1u64..).zip(ids()).zip(sizes);
    let events = events.map(|((sequence, id), &(data, metadata))| NewEvent {
        sequence,
        id,
        name: format!("e.{sequence}"),
        data: vec![b'1'; data],
        metadata: metadata.map(|n| vec![b' '; n]),
        version: 1,
        idempotency_key: None,
    });
    let record = Record {
        time: NOW,
        events: events.collect(),
    };
    fold(&mut env.store, &record, &by()).expect("in memory");
    env
}

const KIB: usize = 1024;
const MIB: usize = 1024 * KIB;

#[test]
fn a_page_of_events_stops_before_its_data_passes_3_mib() {
    let env = with_sizes(&[(512 * KIB, None); 10]);
    let first = list(&env, None, None);
    assert_eq!(names(&first), ["e.10", "e.9", "e.8", "e.7", "e.6", "e.5"]);
    let last = list(&env, None, first.next.as_deref());
    assert_eq!(names(&last), ["e.4", "e.3", "e.2", "e.1"]);
    assert_eq!(last.next, None);
}

/// The sequences on each page, following `next` to the last; the limit of page `i` is `limit(i)`.
fn walk(env: &Env, mut limit: impl FnMut(usize) -> Option<u32>) -> Vec<Vec<u64>> {
    let mut pages = Vec::new();
    let mut cursor = None;
    loop {
        let page = list(env, limit(pages.len()), cursor.as_deref());
        // So the page of the oldest event is the last one.
        assert_eq!(page.events.is_empty(), env.store.events.is_empty());
        pages.push(page.events.iter().map(|e| e.sequence).collect());
        assert!(pages.len() <= env.store.events.len().max(1), "{pages:?}");
        cursor = page.next;
        if cursor.is_none() {
            return pages;
        }
    }
}

#[test]
fn a_page_holds_exactly_3_mib_and_not_a_byte_more_unless_one_event_does() {
    for (sizes, pages) in [
        // Data and metadata together: exactly the bound, with and without more events.
        (
            vec![(1, None), (MIB, None), (2 * MIB - 1, Some(1))],
            vec![vec![3, 2], vec![1]],
        ),
        (
            vec![(MIB, Some(0)), (2 * MIB - 1, Some(1))],
            vec![vec![2, 1]],
        ),
        // A byte more, of data or of metadata.
        (
            vec![(MIB + 1, None), (2 * MIB - 1, Some(1))],
            vec![vec![2], vec![1]],
        ),
        (
            vec![(MIB, None), (2 * MIB - 1, Some(2))],
            vec![vec![2], vec![1]],
        ),
        // An event past the bound by itself, first, between two and last.
        (vec![(3 * MIB + 1, None)], vec![vec![1]]),
        (vec![(3 * MIB, Some(1))], vec![vec![1]]),
        (
            vec![(1, None), (3 * MIB + 1, None), (1, None)],
            vec![vec![3], vec![2], vec![1]],
        ),
        (
            vec![(3 * MIB, None), (1, None), (1, None)],
            vec![vec![3, 2], vec![1]],
        ),
        // An empty one still fits a full page.
        (vec![(0, None), (3 * MIB, None)], vec![vec![2, 1]]),
        // A small event does not jump one that does not fit.
        (
            vec![(1, None), (2 * MIB, None), (2 * MIB, None)],
            vec![vec![3], vec![2, 1]],
        ),
        (vec![], vec![vec![]]),
    ] {
        assert_eq!(walk(&with_sizes(&sizes), |_| None), pages, "{sizes:?}");
    }
}

/// xorshift64*: the same numbers on every run.
struct Rng(u64);

impl Rng {
    /// Below `n`.
    fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        (self.0.wrapping_mul(0x2545_f491_4f6c_dd1d) >> 33) as usize % n
    }

    /// Often a multiple of 256 KiB, so that pages end exactly at the bound; or a byte off one.
    fn size(&mut self) -> usize {
        match self.below(4) {
            0 => self.below(2000),
            1 => (self.below(14) * 256 * KIB + 1).saturating_sub(self.below(3)),
            _ => self.below(5) * 256 * KIB,
        }
    }
}

#[test]
fn the_count_and_the_bytes_of_a_page_together_never_skip_or_repeat_an_event() {
    let mut full = [0; 3];
    for seed in 1..=150 {
        let mut rng = Rng(seed);
        // One case in three has events so small that only the count limits a page.
        let (n, small) = match rng.below(3) {
            0 => (rng.below(450), true),
            _ => (rng.below(30), false),
        };
        let sizes: Vec<_> = (0..n)
            .map(|_| {
                if small {
                    return (rng.below(9), None);
                }
                let data = rng.size();
                let metadata = (rng.below(3) == 0).then(|| rng.below(3) * 32 * KIB);
                // Moved from the data, so that the sums stay near the bound.
                (data.saturating_sub(metadata.unwrap_or(0)), metadata)
            })
            .collect();
        let limits: Vec<_> = (0..=n)
            .map(|_| match rng.below(6) {
                0 => None,
                1 => Some(1 + rng.below(6) as u32),
                2 => Some(199 + rng.below(3) as u32),
                3 => Some(u32::MAX),
                _ => Some(5 + rng.below(30) as u32),
            })
            .collect();
        let env = with_sizes(&sizes);
        let pages = walk(&env, |i| limits[i]);
        let case = format!("seed {seed}: {sizes:?} {limits:?} {pages:?}");
        // Every event once, newest first.
        let newest_first: Vec<u64> = (1..=n as u64).rev().collect();
        assert_eq!(pages.concat(), newest_first, "{case}");
        let bytes = |s: u64| size(&env.store.events[&s]);
        for (i, page) in pages.iter().enumerate() {
            let limit = limits[i].map_or(20, |l| l.min(200)) as usize;
            let total: usize = page.iter().copied().map(bytes).sum();
            assert!(page.len() <= limit, "page {i} of {case}");
            assert!(page.len() <= 1 || total <= 3 * MIB, "page {i} of {case}");
            // It stopped only for a limit it had reached.
            match page.last() {
                Some(&last) if last > 1 && page.len() == limit => full[0] += 1,
                Some(&last) if last > 1 => {
                    assert!(total + bytes(last - 1) > 3 * MIB, "page {i} of {case}");
                    full[1] += 1;
                }
                // The last page: nothing is left.
                _ => full[2] += usize::from(total == 3 * MIB),
            }
        }
    }
    // Each kind of page was seen: ended by the count, by the bytes, and by nothing at the bound.
    assert!(full.iter().all(|&n| n > 0), "{full:?}");
}

/// A view of which events up to `past` cannot be read: the newest of them has a size, and no
/// older one can be looked at.
struct FailsPast<'a> {
    store: &'a Store,
    past: u64,
}

impl EventsView for FailsPast<'_> {
    type Error = u64;
    fn event_by_key(&self, _: &str) -> Result<Option<EventId>, u64> {
        unreachable!()
    }
    fn last_sequence(&self) -> Result<u64, u64> {
        unreachable!()
    }
    fn event(&self, _: EventId) -> Result<Option<Event>, u64> {
        unreachable!()
    }
    fn events_before(
        &self,
        before: Option<u64>,
        budget: &mut PageBudget,
    ) -> Result<Vec<Event>, u64> {
        let mut events = Vec::new();
        for (&sequence, event) in self.store.events.iter().rev() {
            if before.is_some_and(|b| sequence >= b) {
                continue;
            }
            if sequence < self.past {
                return Err(sequence);
            }
            if !budget.take(size(event)) {
                break;
            }
            if sequence == self.past {
                return Err(sequence);
            }
            events.push(event.clone());
        }
        Ok(events)
    }
}

#[test]
fn a_view_that_fails_past_a_page_does_not_fail_it() {
    let env = with_sizes(&[(512 * KIB, None); 10]);
    // By its bytes, then by its count.
    for (limit, past) in [(None, 4), (NonZeroU32::new(3), 7)] {
        let view = FailsPast {
            store: &env.store,
            past,
        };
        let page = list_events(&view, token(1), limit, None).expect("listed");
        let listed: Vec<_> = page.events.iter().map(|e| e.sequence).collect();
        assert_eq!(listed, (past + 1..=10).rev().collect::<Vec<_>>());
        let next = list_events(&view, token(1), limit, page.next.as_deref());
        assert_eq!(next, Err(Error::Store(past)));
    }
}

/// A view that asks on after a refusal, about every event there is, by the sizes it is given.
struct AsksOn<'a> {
    store: &'a Store,
    newest_first: &'a [usize],
}

impl EventsView for AsksOn<'_> {
    type Error = Infallible;
    fn event_by_key(&self, _: &str) -> Result<Option<EventId>, Infallible> {
        unreachable!()
    }
    fn last_sequence(&self) -> Result<u64, Infallible> {
        unreachable!()
    }
    fn event(&self, _: EventId) -> Result<Option<Event>, Infallible> {
        unreachable!()
    }
    fn events_before(
        &self,
        _: Option<u64>,
        budget: &mut PageBudget,
    ) -> Result<Vec<Event>, Infallible> {
        let events = self.store.events.values().rev().zip(self.newest_first);
        Ok(events
            .filter(|&(_, &size)| budget.take(size))
            .map(|(e, _)| e.clone())
            .collect())
    }
}

#[test]
fn a_refused_event_ends_the_page_for_every_older_one() {
    let env = with_events(3);
    for (newest_first, limit) in [
        // By its count, then by its bytes, with a small event after the one that does not fit.
        ([1, 1, 1], NonZeroU32::new(1)),
        ([1, 3 * MIB, 1], None),
        // Their sum overflows.
        ([1, usize::MAX, 1], None),
        ([usize::MAX, 1, 1], None),
    ] {
        let view = AsksOn {
            store: &env.store,
            newest_first: &newest_first,
        };
        let page = list_events(&view, token(1), limit, None).expect("listed");
        assert_eq!(names(&page), ["e.3"], "{newest_first:?}");
        assert_eq!(page.next, Some(format!("{}.3", token(1))));
    }
}

/// A store whose one event takes the last event sequence there is.
fn at_last_sequence() -> Env {
    let mut env = Env::new();
    let record = Record {
        time: NOW,
        events: vec![NewEvent {
            sequence: u64::MAX,
            id: id(1),
            name: "last".into(),
            data: "null".into(),
            metadata: None,
            version: 1,
            idempotency_key: None,
        }],
    };
    fold(&mut env.store, &record, &by()).expect("in memory");
    env
}

#[test]
fn an_event_at_the_last_sequence_is_listed() {
    let env = at_last_sequence();
    assert_eq!(names(&list(&env, None, None)), ["last"]);
}

#[test]
fn an_emit_past_the_last_sequence_is_refused() {
    let mut env = at_last_sequence();
    assert!(matches!(env.emit(named("next")), Err(Error::Refused(_))));
    assert_eq!(env.stored(), ["last"]);
}

#[test]
fn a_cursor_is_stable_while_new_events_arrive() {
    let mut env = with_events(4);
    let first = list(&env, Some(2), None);
    for _ in 0..3 {
        env.emit(named("later")).expect("accepted");
    }
    let second = list(&env, Some(2), first.next.as_deref());
    assert_eq!(names(&second), ["e.2", "e.1"]);
}

#[test]
fn a_cursor_that_does_not_decode_or_names_another_incarnation_is_invalid() {
    let env = with_events(3);
    let next = list(&env, Some(1), None).next.expect("more pages");
    let mut bad = vec![
        String::new(),
        "not-a-token".into(),
        format!("{next}x"),
        format!("+{next}"),
        next.replace('.', ".0"),
    ];
    let other = list_events(&env.store, token(2), NonZeroU32::new(1), None)
        .expect("listed")
        .next
        .expect("more pages");
    bad.push(other);
    for cursor in &bad {
        match list_events(&env.store, token(1), None, Some(cursor)) {
            Err(Error::Refused(r)) => assert_eq!(r.kind(), ErrorKind::Invalid),
            other => panic!("expected invalid for {cursor:?}, got {other:?}"),
        }
    }
}

// ---- The engine's events ----

/// RUN-14: only `ironflowx-execution` makes engine events; this helper stands in for it.
#[expect(clippy::disallowed_methods)]
fn made(event: EngineEvent) -> EngineEmit {
    engine_event(event)
}

const RUN: &str = "019a9f3e-7c2b-7d41-8a3e-5f6b7c8d9e0f";

fn completed(function: &str, code: &str) -> EngineEvent {
    EngineEvent::RunCompleted {
        run_id: RUN.parse().expect("a run ID"),
        function: function.into(),
        code: code.into(),
    }
}

fn failed(error_code: &str, error_message: &str) -> EngineEvent {
    EngineEvent::RunFailed {
        run_id: RUN.parse().expect("a run ID"),
        function: "charge".into(),
        code: "v1".into(),
        error_code: error_code.into(),
        error_message: error_message.into(),
    }
}

/// Sequences engine events in `env` and folds them in one record, as the ledger would.
fn append(env: &mut Env, emits: Vec<EngineEmit>) -> Vec<NewEvent> {
    let events = sequence(&env.store, emits, &mut env.fresh).expect("sequenced");
    let record = Record {
        time: NOW,
        events: events.clone(),
    };
    fold(&mut env.store, &record, &by()).expect("in memory");
    events
}

#[test]
fn engine_events_follow_the_stored_ones_with_version_1_and_nothing_a_client_sets() {
    let mut env = Env::new();
    env.emit(named("order.placed")).expect("emitted");
    env.emit(named("order.paid")).expect("emitted");
    let emits = vec![
        made(completed("charge", "v1")),
        made(failed("ironflowx.step_order", "expected \"pay\"")),
    ];
    let completed_data = format!(r#"{{"run_id":"{RUN}","function":"charge","code":"v1"}}"#);
    let failed_data = format!(
        r#"{{"run_id":"{RUN}","function":"charge","code":"v1","error":{{"code":"ironflowx.step_order","message":"expected \"pay\""}}}}"#
    );
    let event = |n: u32, name: &str, data: &str| NewEvent {
        sequence: u64::from(n),
        id: id(n),
        name: name.into(),
        data: data.into(),
        metadata: None,
        version: 1,
        idempotency_key: None,
    };
    assert_eq!(
        append(&mut env, emits),
        [
            event(3, "ironflowx.run.completed", &completed_data),
            event(4, "ironflowx.run.failed", &failed_data),
        ]
    );
    let read = get_event(&env.store, &id(4).to_string()).expect("readable");
    assert_eq!((read.time, read.emitted_by), (NOW, by()));
    // The next client event takes the next sequence.
    env.emit(named("order.shipped")).expect("emitted");
    assert_eq!(
        env.store.events.keys().copied().collect::<Vec<_>>(),
        [1, 2, 3, 4, 5]
    );
}

#[test]
fn engine_event_data_is_json_that_event_rules_accept_and_holds_each_value() {
    let awkward = "quote \" backslash \\ newline \n tab \t é 💸 \u{1}";
    for (event, error) in [
        (completed(awkward, awkward), None),
        (failed(awkward, awkward), Some(awkward)),
    ] {
        let [new] = sequence(&Store::default(), vec![made(event)], &mut ids())
            .expect("sequenced")
            .try_into()
            .expect("one event");
        let emit = Emit {
            data: Some(new.data.clone()),
            ..named("order.placed")
        };
        assert_eq!(check_emit(&emit), Ok(()));
        let data: serde_json::Value = serde_json::from_slice(&new.data).expect("JSON");
        let mut expected = serde_json::json!({"run_id": RUN, "function": awkward, "code": awkward});
        if let Some(e) = error {
            expected["code"] = "v1".into();
            expected["function"] = "charge".into();
            expected["error"] = serde_json::json!({"code": e, "message": e});
        }
        assert_eq!(data, expected);
    }
}

/// A view that fails the test if it is read.
struct Unread;

impl EventsView for Unread {
    type Error = Infallible;
    fn event_by_key(&self, _: &str) -> Result<Option<EventId>, Infallible> {
        unreachable!("read")
    }
    fn last_sequence(&self) -> Result<u64, Infallible> {
        unreachable!("read")
    }
    fn event(&self, _: EventId) -> Result<Option<Event>, Infallible> {
        unreachable!("read")
    }
    fn events_before(&self, _: Option<u64>, _: &mut PageBudget) -> Result<Vec<Event>, Infallible> {
        unreachable!("read")
    }
}

#[test]
fn no_engine_event_reads_nothing_and_draws_no_id() {
    let mut fresh = ids();
    assert_eq!(sequence(&Unread, Vec::new(), &mut fresh), Ok(Vec::new()));
    assert_eq!(fresh.next(), Some(id(1)));
}

#[test]
fn engine_events_past_the_last_sequence_are_refused() {
    let mut env = at_last_sequence();
    let refused = sequence(
        &env.store,
        vec![made(completed("charge", "v1"))],
        &mut env.fresh,
    );
    match refused {
        Err(Error::Refused(r)) => {
            let got = (r.kind(), r.message(), r.hint());
            assert_eq!(
                got,
                (ErrorKind::Invalid, "event sequences are used up", None)
            );
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[test]
fn a_client_emit_of_an_engine_events_name_is_refused_as_any_owned_name() {
    for name in ["ironflowx.run.completed", "ironflowx.run.failed"] {
        let message = assert_refused(named(name));
        assert_eq!(message, "event name is owned by the engine", "{name}");
        let mut env = Env::new();
        let batch = Submission::Batch(vec![named(name), named("order.placed")]);
        let outcome = env.submit(batch).expect("decided");
        assert!(
            matches!(&outcome[0], ItemOutcome::Refused(r) if r.message() == message),
            "{outcome:?}"
        );
        assert_eq!(env.stored(), ["order.placed"]);
    }
}
