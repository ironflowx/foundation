//! Event rules through `decide`, `fold` and the queries, over an in-memory store.

// D17: only the ledger stamps event time; these tests stand in for it, and only they may.
#![expect(clippy::disallowed_methods)]

use std::collections::BTreeMap;
use std::convert::Infallible;

use std::num::NonZeroU32;

use ironflowx_core::{ErrorKind, EventId, IncarnationToken, SequencedTime, Timestamp};
use ironflowx_events::{
    Decision, Emit, Error, Event, EventPage, EventTables, EventsView, ItemOutcome, Submission,
    decide, fold, get_event, list_events,
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
    fn events_before(&self, before: u64, limit: usize) -> Result<Vec<Event>, Infallible> {
        Ok(self
            .events
            .range(..before)
            .rev()
            .take(limit)
            .map(|(_, e)| e.clone())
            .collect())
    }
}

impl EventTables for Store {
    type Error = Infallible;
    fn insert(&mut self, event: Event) -> Result<(), Infallible> {
        self.events.insert(event.sequence, event);
        Ok(())
    }
}

const NOW: Timestamp = Timestamp::from_millis(1_700_000_000_000);

/// The event-clock reading every test decides at.
fn now() -> SequencedTime {
    SequencedTime::from_event_clock(NOW)
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
            fold(&mut self.store, &record).expect("in memory");
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
        r#"{"n": 1.10, "big": 123456789012345678901234567890}"#,
        r#"{  "spaced" :  true  }"#,
        &nested_arrays(127),
    ] {
        assert_eq!(assert_accepted(with_data(data)).data, data);
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
    ] {
        assert_refused(with_data(data));
    }
    assert_accepted(with_data(r#"{"inner": {"$redacted": true}}"#));
    assert_accepted(with_data(r#"[{"$redacted": true}]"#));
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
        Some(at_limit)
    );
    assert_accepted(with_metadata(r#"{"$redacted": 1}"#));
    for metadata in [
        &blob("t", 65_537),
        r#"["t"]"#,
        "null",
        r#""t""#,
        r#"{"t": 1, "t": 2}"#,
        r#"{"t": "x\u0000"}"#,
        r#"{"t": "\udc00"}"#,
        r#"{"t": 1"#,
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
    assert_ne!(
        refusal_of(env.submit(Submission::Batch(vec![big.clone(); 9]))),
        ""
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
        fold(&mut env.store, &record).expect("in memory");
        records.push(record);
    }
    let mut rebuilt = Store::default();
    for record in &records {
        fold(&mut rebuilt, record).expect("in memory");
    }
    assert_eq!(rebuilt, env.store);
}

#[test]
fn folding_a_record_onto_a_read_store_that_holds_it_leaves_the_rows_unchanged() {
    let mut env = Env::new();
    let sub = Submission::Batch(vec![with_key("a", "k"), named("b")]);
    let record = decide(&env.store, &sub, now(), &mut env.fresh)
        .expect("accepted")
        .record
        .expect("new events");
    fold(&mut env.store, &record).expect("in memory");
    let once = env.store.events.clone();
    // The ledger skips applied positions (Spec 4); a store keyed by event sequence, as the
    // read store is, keeps one row per event even if a record reaches fold again.
    fold(&mut env.store, &record).expect("in memory");
    assert_eq!(env.store.events, once);
    assert_eq!(env.stored(), ["a", "b"]);
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
