//! The deterministic tests of invariants.md, through `Ledger`.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

use ironflowx_core::{Emitter, EnvRef, ErrorKind, EventId, SubmitAs, Timestamp};
use ironflowx_events::{Emit, Event, ItemOutcome, Submission as Events};
use segment_log_core::{ExpectedRevision, NewRecord, Options, SegmentLog, Submit};

use crate::record::Entries;
use crate::store::ReadStore;
use crate::{
    Ledger, LedgerState, ManualClock, OpenError, Outcome, ReadError, Submission, SubmitError,
    new_env_ref, record,
};

const T: i64 = 1_800_000_000_000;
const HOUR: i64 = 3_600_000;

fn clock() -> ManualClock {
    ManualClock::new(Timestamp::from_millis(T))
}

/// What the tests submit as, unless one names another embedder.
fn embedder() -> SubmitAs {
    SubmitAs::embedder("ledger-tests").unwrap()
}

/// How those submissions read back.
fn emitter() -> Emitter {
    "embedder:ledger-tests".parse().unwrap()
}

async fn open(dir: &Path, env: EnvRef) -> Ledger {
    Ledger::open(dir, env, clock()).await.expect("opens")
}

fn named(name: &str) -> Emit {
    Emit {
        name: name.into(),
        ..Emit::default()
    }
}

fn keyed(name: &str, key: &str) -> Emit {
    Emit {
        idempotency_key: Some(key.into()),
        ..named(name)
    }
}

/// The outcomes of a submission of events.
fn items(outcome: Outcome) -> Vec<ItemOutcome> {
    match outcome {
        Outcome::Events(items) => items,
        Outcome::Execution(o) => panic!("events answered {o:?}"),
    }
}

/// A record of events and no execution entry, encoded.
fn events(
    token: ironflowx_core::IncarnationToken,
    id: uuid::Uuid,
    emitted_by: &Emitter,
    record: &ironflowx_events::Record,
) -> Vec<u8> {
    let entries = Entries {
        record: record.clone(),
        execution: Vec::new(),
    };
    record::entries(token, id, emitted_by, &entries)
}

async fn outcome(ledger: &Ledger, emit: Emit) -> Result<ItemOutcome, SubmitError> {
    let committed = ledger
        .submit(Submission::Events(Events::Emit(emit)), embedder())
        .await?;
    let [outcome] = <[_; 1]>::try_from(items(committed.outcome)).expect("one outcome per emit");
    Ok(outcome)
}

async fn emit(ledger: &Ledger, emit: Emit) -> Result<EventId, SubmitError> {
    match outcome(ledger, emit).await? {
        ItemOutcome::New(id) | ItemOutcome::Duplicate(id) => Ok(id),
        refused @ ItemOutcome::Refused(_) => panic!("an emit answered {refused:?}"),
    }
}

async fn get(ledger: &Ledger, id: EventId) -> Result<Event, ReadError> {
    ledger.get_event(&id.to_string()).await
}

async fn all(ledger: &Ledger) -> Result<Vec<Event>, ReadError> {
    ledger
        .list_events(200.try_into().ok(), None)
        .await
        .map(|p| p.events)
}

/// The event rows as a client sees them.
async fn names(ledger: &Ledger) -> Vec<String> {
    all(ledger)
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.name)
        .collect()
}

#[tokio::test]
async fn a_read_waits_for_a_held_fold_then_finds_the_event() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = open(dir.path(), new_env_ref()).await;
    // The very first event of a fresh ledger, then a later one.
    for name in ["first", "second"] {
        ledger.hold_fold();
        let id = emit(&ledger, named(name)).await.unwrap();
        let read = async { (get(&ledger, id).await, all(&ledger).await) };
        tokio::pin!(read);
        assert!(
            tokio::time::timeout(Duration::from_millis(100), &mut read)
                .await
                .is_err(),
            "the read waits for the fold"
        );
        ledger.release_fold();
        let (event, page) = read.await;
        assert_eq!(event.unwrap().name, name);
        assert_eq!(page.unwrap()[0].id, id);
    }
}

#[tokio::test]
async fn event_time_follows_the_clock_and_never_goes_back() {
    let dir = tempfile::tempdir().unwrap();
    let env = new_env_ref();
    let clock = clock();
    let ledger = Ledger::open(dir.path(), env, clock.clone()).await.unwrap();
    let time = async |ledger: &Ledger| {
        let id = emit(ledger, named("tick")).await.unwrap();
        get(ledger, id).await.unwrap().time.as_millis()
    };
    assert_eq!(time(&ledger).await, T);
    clock.step(HOUR);
    assert_eq!(time(&ledger).await, T + HOUR);
    clock.step(-2 * HOUR);
    assert_eq!(time(&ledger).await, T + HOUR);
    ledger.close().await;
    let ledger = Ledger::open(dir.path(), env, clock.clone()).await.unwrap();
    assert_eq!(time(&ledger).await, T + HOUR);
}

#[tokio::test]
async fn every_event_of_one_batch_carries_the_record_time() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = open(dir.path(), new_env_ref()).await;
    let batch = Submission::Events(Events::Batch(vec![named("a"), named("b")]));
    ledger.submit(batch, embedder()).await.unwrap();
    let times: Vec<_> = all(&ledger).await.unwrap().iter().map(|e| e.time).collect();
    assert_eq!(times, [Timestamp::from_millis(T); 2]);
}

#[tokio::test]
async fn replaying_folds_each_record_exactly_once() {
    let dir = tempfile::tempdir().unwrap();
    let env = new_env_ref();
    let ledger = open(dir.path(), env).await;
    for name in ["a", "b", "c"] {
        emit(&ledger, keyed(name, name)).await.unwrap();
    }
    let before = all(&ledger).await.unwrap();
    // Feed records at or below the applied position again.
    {
        let mut conn = ledger.store.lock().unwrap();
        let applied = crate::store::applied(&conn).unwrap();
        let mut record = ironflowx_events::Record {
            time: Timestamp::from_millis(T),
            events: vec![],
        };
        for offset in 0..applied {
            // An event no row holds: folding it would add one.
            record.events = vec![ironflowx_events::NewEvent {
                sequence: 100 + offset,
                id: uuid::Uuid::now_v7().to_string().parse().unwrap(),
                name: "again".into(),
                data: b"null".to_vec(),
                metadata: None,
                version: 1,
                idempotency_key: None,
            }];
            let id = uuid::Uuid::now_v7();
            let entries = Entries {
                record: record.clone(),
                execution: Vec::new(),
            };
            let again = Some((id, &entries, &emitter()));
            crate::store::apply(&mut conn, offset, again, false).unwrap();
            assert_eq!(crate::store::applied(&conn).unwrap(), applied);
        }
    }
    assert_eq!(all(&ledger).await.unwrap(), before);
    ledger.close().await;
    std::fs::remove_file(dir.path().join("read.db")).unwrap();
    let ledger = open(dir.path(), env).await;
    assert_eq!(all(&ledger).await.unwrap(), before);
    assert_eq!(emit(&ledger, keyed("x", "a")).await.unwrap(), before[2].id);
}

#[tokio::test]
async fn a_restart_keeps_every_acknowledged_event() {
    let dir = tempfile::tempdir().unwrap();
    let env = new_env_ref();
    let ledger = open(dir.path(), env).await;
    emit(&ledger, named("a")).await.unwrap();
    emit(&ledger, named("b")).await.unwrap();
    ledger.close().await;
    let ledger = open(dir.path(), env).await;
    assert_eq!(names(&ledger).await, ["b", "a"]);
}

#[tokio::test]
async fn another_incarnation_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let env = new_env_ref();
    open(dir.path(), env).await.close().await;
    let same_id: EnvRef = format!("{}.{}", env.id(), new_env_ref().token())
        .parse()
        .unwrap();
    for other in [same_id, new_env_ref()] {
        let err = Ledger::open(dir.path(), other, clock()).await.err();
        assert!(matches!(err, Some(OpenError::WrongIncarnation)), "{err:?}");
    }
}

#[tokio::test]
async fn a_genesis_of_another_format_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let env = new_env_ref();
    {
        let log = SegmentLog::open(dir.path().join("log"), Options::default()).unwrap();
        let mut genesis = record::genesis(env);
        genesis[1..3].copy_from_slice(&(record::FORMAT + 1).to_le_bytes());
        append(&log, ExpectedRevision::Empty, genesis);
    }
    let err = Ledger::open(dir.path(), env, clock()).await.err();
    assert!(matches!(err, Some(OpenError::Format)), "{err:?}");
}

#[tokio::test]
async fn a_later_record_with_another_token_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let env = new_env_ref();
    open(dir.path(), env).await.close().await;
    {
        let log = SegmentLog::open(dir.path().join("log"), Options::default()).unwrap();
        let revision = log.stream_head(b"events").unwrap().revision;
        let record = ironflowx_events::Record {
            time: Timestamp::from_millis(T),
            events: vec![],
        };
        let foreign = events(
            new_env_ref().token(),
            uuid::Uuid::now_v7(),
            &emitter(),
            &record,
        );
        append(&log, ExpectedRevision::At(revision), foreign);
    }
    std::fs::remove_file(dir.path().join("read.db")).unwrap();
    let err = Ledger::open(dir.path(), env, clock()).await.err();
    assert!(matches!(err, Some(OpenError::Corrupt)), "{err:?}");
}

#[tokio::test]
async fn a_later_record_that_does_not_decode_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let env = new_env_ref();
    open(dir.path(), env).await.close().await;
    {
        let log = SegmentLog::open(dir.path().join("log"), Options::default()).unwrap();
        let revision = log.stream_head(b"events").unwrap().revision;
        append(&log, ExpectedRevision::At(revision), vec![0xFF]);
    }
    std::fs::remove_file(dir.path().join("read.db")).unwrap();
    let err = Ledger::open(dir.path(), env, clock()).await.err();
    assert!(matches!(err, Some(OpenError::Corrupt)), "{err:?}");
}

#[tokio::test]
async fn a_log_without_a_genesis_record_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let env = new_env_ref();
    {
        let log = SegmentLog::open(dir.path().join("log"), Options::default()).unwrap();
        let record = ironflowx_events::Record {
            time: Timestamp::from_millis(T),
            events: vec![],
        };
        append(
            &log,
            ExpectedRevision::Empty,
            events(env.token(), uuid::Uuid::now_v7(), &emitter(), &record),
        );
    }
    let err = Ledger::open(dir.path(), env, clock()).await.err();
    assert!(matches!(err, Some(OpenError::Corrupt)), "{err:?}");
    // To an open that asks for a ledger already there, one without its genesis record is not it.
    let err = Ledger::open_existing_blocking(dir.path(), env, clock()).err();
    assert!(matches!(err, Some(OpenError::Missing)), "{err:?}");
}

#[tokio::test]
async fn an_empty_ledger_reopens_after_its_read_store_is_deleted() {
    let dir = tempfile::tempdir().unwrap();
    let env = new_env_ref();
    open(dir.path(), env).await.close().await;
    std::fs::remove_file(dir.path().join("read.db")).unwrap();
    let ledger = open(dir.path(), env).await;
    assert!(all(&ledger).await.unwrap().is_empty());
    emit(&ledger, named("a")).await.unwrap();
}

#[tokio::test]
async fn a_read_store_of_another_incarnation_is_rebuilt() {
    let dir = tempfile::tempdir().unwrap();
    let env = new_env_ref();
    let ledger = open(dir.path(), env).await;
    emit(&ledger, named("mine")).await.unwrap();
    ledger.close().await;
    // Swap in the read store of another directory.
    let other = tempfile::tempdir().unwrap();
    let ledger = open(other.path(), new_env_ref()).await;
    emit(&ledger, named("theirs")).await.unwrap();
    ledger.close().await;
    std::fs::copy(other.path().join("read.db"), dir.path().join("read.db")).unwrap();
    let ledger = open(dir.path(), env).await;
    assert_eq!(names(&ledger).await, ["mine"]);
}

#[tokio::test]
async fn a_read_store_with_damaged_tables_is_rebuilt() {
    // With something to replay, and with nothing: `meta` agrees with the genesis record
    // either way.
    for damage in [
        "DROP TABLE events; UPDATE meta SET applied = 1",
        "DROP TABLE events",
        "DROP TABLE idempotency",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let env = new_env_ref();
        let ledger = open(dir.path(), env).await;
        let id = emit(&ledger, keyed("a", "k")).await.unwrap();
        ledger.close().await;
        let conn = rusqlite::Connection::open(dir.path().join("read.db")).unwrap();
        conn.execute_batch(damage).unwrap();
        drop(conn);
        let ledger = open(dir.path(), env).await;
        assert_eq!(names(&ledger).await, ["a"], "{damage}");
        let retry = outcome(&ledger, keyed("b", "k")).await.unwrap();
        assert_eq!(retry, ItemOutcome::Duplicate(id), "{damage}");
    }
}

/// The same of each execution table: a store that lacks one, or a column one is read by, is
/// rebuilt, and answers as before.
#[tokio::test]
async fn a_read_store_without_an_execution_table_is_rebuilt() {
    use ironflowx_execution::{Begun, Command, Outcome as Answer};
    for damage in [
        "DROP TABLE functions",
        "DROP TABLE triggers",
        "DROP TABLE runs",
        "DROP TABLE steps",
        // A column a lookup reads but does not return.
        "ALTER TABLE steps RENAME COLUMN run_id TO lost",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let env = new_env_ref();
        let ledger = open(dir.path(), env).await;
        execute(&ledger, put("charge", "v1", &["order.placed"]))
            .await
            .unwrap();
        let run = started(&ledger, "charge").await;
        claim_of(&ledger, &["charge@v1"]).await.unwrap();
        let begin = Command::BeginStep {
            lease: format!("{run}.1"),
            position: 1,
            name: "reserve".into(),
            once: false,
            input: None,
        };
        execute(&ledger, begin.clone()).await.unwrap();
        let function = ledger.get_function("charge").await.unwrap();
        let read = ledger.get_run(&run.to_string()).await.unwrap();
        ledger.close().await;
        let conn = rusqlite::Connection::open(dir.path().join("read.db")).unwrap();
        conn.execute_batch(damage).unwrap();
        drop(conn);
        let ledger = open(dir.path(), env).await;
        assert_eq!(
            ledger.get_function("charge").await.unwrap(),
            function,
            "{damage}"
        );
        assert_eq!(
            ledger.get_run(&run.to_string()).await.unwrap(),
            read,
            "{damage}"
        );
        let open_step = Answer::StepBegun(Begun::Run { attempt: 1 });
        assert_eq!(
            execute(&ledger, begin).await.unwrap(),
            open_step,
            "{damage}"
        );
        ledger.close().await;
    }
}

#[tokio::test]
async fn a_panicking_query_fails_the_ledger_and_reopen_recovers() {
    let dir = tempfile::tempdir().unwrap();
    let env = new_env_ref();
    let ledger = open(dir.path(), env).await;
    emit(&ledger, named("a")).await.unwrap();
    let answer = ledger
        .read(
            |_| -> Result<(), ironflowx_events::Error<crate::store::StoreError>> {
                panic!("a query bug")
            },
        )
        .await;
    assert_eq!(answer, Err(ReadError::Unavailable));
    assert_eq!(all(&ledger).await.unwrap_err(), ReadError::Unavailable);
    assert_eq!(
        emit(&ledger, named("b")).await.unwrap_err(),
        SubmitError::Unavailable
    );
    ledger.close().await;
    let ledger = open(dir.path(), env).await;
    assert_eq!(names(&ledger).await, ["a"]);
}

#[tokio::test]
async fn a_cursor_past_every_sequence_lists_from_the_newest() {
    let dir = tempfile::tempdir().unwrap();
    let env = new_env_ref();
    let ledger = open(dir.path(), env).await;
    emit(&ledger, named("a")).await.unwrap();
    let cursor = format!("{}.{}", env.token(), u64::MAX);
    let page = ledger
        .read(move |v| ironflowx_events::list_events(v, env.token(), None, Some(&cursor)))
        .await
        .unwrap();
    assert_eq!(page.events.len(), 1);
}

fn store_message(e: ironflowx_events::Error<crate::store::StoreError>) -> String {
    match e {
        ironflowx_events::Error::Store(e) => e.to_string(),
        ironflowx_events::Error::Refused(r) => panic!("refused: {r:?}"),
    }
}

#[tokio::test]
async fn a_page_of_events_reads_no_event_past_its_end() {
    // Ten events of 512 KiB, their metadata included, so six are 3 MiB exactly; or the newest is
    // a byte larger.
    for (metadata, larger, limit, first, second, third) in [
        (
            "",
            0,
            None,
            vec![10, 9, 8, 7, 6, 5],
            vec![4, 3, 2, 1],
            false,
        ),
        (
            "{}",
            0,
            None,
            vec![10, 9, 8, 7, 6, 5],
            vec![4, 3, 2, 1],
            false,
        ),
        (
            "{}",
            1,
            None,
            vec![10, 9, 8, 7, 6],
            vec![5, 4, 3, 2, 1],
            false,
        ),
        ("", 0, 2.try_into().ok(), vec![10, 9], vec![8, 7], true),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let env = new_env_ref();
        let ledger = open(dir.path(), env).await;
        // Each event's data is its own letter, so that its bytes can be found in the file.
        for letter in 'a'..='j' {
            let larger = if letter == 'j' { larger } else { 0 };
            // Less the quotes.
            let letters = letter.to_string().repeat(524_286 - metadata.len() + larger);
            let big = Emit {
                data: Some(format!("\"{letters}\"").into_bytes()),
                metadata: (!metadata.is_empty()).then(|| metadata.into()),
                ..named("blob.saved")
            };
            emit(&ledger, big).await.unwrap();
        }
        ledger.close().await;
        let path = dir.path().join("read.db");
        let list = |cursor: Option<&str>| {
            let conn = rusqlite::Connection::open(&path).unwrap();
            let page = ironflowx_events::list_events(&ReadStore(&conn), env.token(), limit, cursor);
            page.map(|p| {
                let sequences: Vec<u64> = p.events.iter().map(|e| e.sequence).collect();
                (sequences, p.next)
            })
        };
        let (listed, next) = list(None).unwrap();
        assert_eq!(listed, first);
        let after = list(next.as_deref()).unwrap();
        let third = third.then(|| format!("{}.{}", env.token(), second[second.len() - 1]));
        assert_eq!(after, (second.clone(), third));

        // Cut the chain of pages that holds the data of every event past the first page: SQLite
        // fails to read any of them now, and the first page lists all the same.
        let page_size: usize = rusqlite::Connection::open(&path)
            .and_then(|conn| conn.query_row("PRAGMA page_size", [], |r| r.get(0)))
            .unwrap();
        let mut file = std::fs::read(&path).unwrap();
        let past_letters: BTreeSet<u8> = (b'a'..).zip(1..=second[0]).map(|(l, _)| l).collect();
        let mut cut = BTreeSet::new();
        for page in file.chunks_mut(page_size) {
            let letter = page[4];
            let (next, data) = page.split_at_mut(4);
            if past_letters.contains(&letter) && data.iter().all(|&b| b == letter) {
                next.fill(0xff);
                cut.insert(letter);
            }
        }
        assert_eq!(cut, past_letters);
        std::fs::write(&path, file).unwrap();
        assert_eq!(list(None).unwrap(), (first, next.clone()));
        let err = list(next.as_deref()).unwrap_err();
        assert_eq!(
            store_message(err),
            "read store: database disk image is malformed"
        );
    }
}

#[test]
fn a_page_fails_when_the_read_store_cannot_read_an_event_on_it() {
    let token = new_env_ref().token();
    for (schema, failed) in [
        ("", "no such table: events"),
        // The size is readable and the event is not.
        (
            "CREATE TABLE events (sequence, data, metadata)",
            "no such column: id",
        ),
        (
            "CREATE VIEW events AS SELECT -1 AS sequence, 'x' AS id, 'x' AS name, x'' AS data,
             NULL AS metadata, 1 AS version, NULL AS idempotency_key, 0 AS time, '' AS emitted_by",
            "Integer -1 out of range at index 0",
        ),
        // The size cannot be read: no integer, then an error from SQLite itself.
        (
            "CREATE VIEW events AS SELECT 1 AS sequence, 'x' AS id, 'x' AS name, NULL AS data,
             NULL AS metadata, 1 AS version, NULL AS idempotency_key, 0 AS time, '' AS emitted_by",
            "Invalid column type Null at index: 1",
        ),
        (
            "CREATE VIEW events AS SELECT 1 AS sequence, 'x' AS id, 'x' AS name,
             abs(-9223372036854775808) AS data, NULL AS metadata, 1 AS version,
             NULL AS idempotency_key, 0 AS time, '' AS emitted_by",
            "integer overflow",
        ),
    ] {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(schema).unwrap();
        let page = ironflowx_events::list_events(&ReadStore(&conn), token, None, None);
        let message = store_message(page.unwrap_err());
        // Less what rusqlite adds: the statement, or the column's name.
        let cause = message.split(" in SELECT").next().unwrap();
        let cause = cause.split(", name: ").next().unwrap();
        assert_eq!(cause, format!("read store: {failed}"));
    }
}

#[test]
fn a_page_looks_at_no_event_older_than_the_one_that_ends_it() {
    let token = new_env_ref().token();
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    // Event 1 has no size.
    conn.execute_batch(
        "CREATE TABLE events (sequence INTEGER PRIMARY KEY, id, name, data, metadata, version,
                              idempotency_key, time, emitted_by);
         INSERT INTO events SELECT column1, '019a8f3c-7e21-7b4a-9c3d-00000000000' || column1,
             'x', column2, NULL, 1, NULL, 0, 'embedder:x'
             FROM (VALUES (1, NULL), (2, x'31'), (3, x'31'));",
    )
    .unwrap();
    let list = |cursor: Option<&str>| {
        ironflowx_events::list_events(&ReadStore(&conn), token, 1.try_into().ok(), cursor)
    };
    let page = list(None).unwrap();
    assert_eq!(page.events.len(), 1);
    assert_eq!(page.next, Some(format!("{token}.3")));
    let err = list(page.next.as_deref()).unwrap_err();
    assert!(store_message(err).starts_with("read store: Invalid column type Null at index: 1,"));
}

#[tokio::test]
async fn a_second_open_is_locked_and_dropping_releases_it() {
    let dir = tempfile::tempdir().unwrap();
    let env = new_env_ref();
    let ledger = open(dir.path(), env).await;
    let err = Ledger::open(dir.path(), env, clock()).await.err();
    assert!(matches!(err, Some(OpenError::Locked)), "{err:?}");
    drop(ledger);
    open(dir.path(), env).await;
}

#[tokio::test]
async fn a_failed_fold_ends_waiting_reads_and_reopen_applies_the_record() {
    let dir = tempfile::tempdir().unwrap();
    let env = new_env_ref();
    let ledger = open(dir.path(), env).await;
    ledger.hold_fold();
    ledger.fail_next_fold();
    let id = emit(&ledger, named("a")).await.unwrap();
    {
        let read = get(&ledger, id);
        tokio::pin!(read);
        assert!(
            tokio::time::timeout(Duration::from_millis(100), &mut read)
                .await
                .is_err()
        );
        ledger.release_fold();
        assert_eq!(read.await.unwrap_err(), ReadError::Unavailable);
    }
    assert_eq!(
        emit(&ledger, named("b")).await.unwrap_err(),
        SubmitError::Unavailable
    );
    ledger.close().await;
    let ledger = open(dir.path(), env).await;
    assert_eq!(get(&ledger, id).await.unwrap().name, "a");
}

#[tokio::test]
async fn an_unknown_append_is_recovered_by_a_keyed_retry() {
    for landed in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let env = new_env_ref();
        let ledger = open(dir.path(), env).await;
        ledger.fail_next_append(landed);
        let err = emit(&ledger, keyed("a", "k")).await.unwrap_err();
        assert_eq!(err, SubmitError::Unknown);
        let err = emit(&ledger, keyed("a", "k")).await.unwrap_err();
        assert_eq!(err, SubmitError::Unavailable);
        assert_eq!(all(&ledger).await.unwrap_err(), ReadError::Unavailable);
        ledger.close().await;

        let ledger = open(dir.path(), env).await;
        let before = all(&ledger).await.unwrap();
        assert_eq!(before.len(), usize::from(landed));
        let retry = outcome(&ledger, keyed("a", "k")).await.unwrap();
        let after = all(&ledger).await.unwrap();
        assert_eq!(after.len(), 1);
        let id = after[0].id;
        let expected = if landed {
            ItemOutcome::Duplicate(id)
        } else {
            ItemOutcome::New(id)
        };
        assert_eq!(retry, expected);
    }
}

#[tokio::test]
async fn a_refused_emit_leaves_nothing_behind() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = open(dir.path(), new_env_ref()).await;
    let err = emit(&ledger, named("a..b")).await.unwrap_err();
    assert!(
        matches!(&err, SubmitError::Refused(r) if r.kind() == ErrorKind::Invalid),
        "{err:?}"
    );
    assert!(all(&ledger).await.unwrap().is_empty());
    // The next event still takes sequence 1: nothing was drawn.
    emit(&ledger, named("a")).await.unwrap();
    assert_eq!(names(&ledger).await, ["a"]);
}

#[test]
fn minted_tokens_are_distinct() {
    let tokens: BTreeSet<_> = (0..10_000)
        .map(|_| new_env_ref().token().to_string())
        .collect();
    assert_eq!(tokens.len(), 10_000);
}

/// One execution entry of each kind, about `run`, as a record holds them. Only execution rules
/// make a `RunCompleted` entry, so that one is read back from its bytes.
fn execution_entries(run: ironflowx_core::RunId) -> Vec<ironflowx_execution::Stored> {
    use ironflowx_execution::{Definition, Entry};
    let mut completed = vec![6];
    completed.extend_from_slice(&36u32.to_le_bytes());
    completed.extend_from_slice(run.to_string().as_bytes());
    completed.extend_from_slice(&1u32.to_le_bytes());
    completed.push(b'7');
    let completed = ironflowx_execution::decode(&mut completed.as_slice()).expect("an entry");
    let entries = vec![
        Entry::FunctionPut(Definition {
            name: "charge".into(),
            code: "v1".into(),
            triggers: vec!["order.paid".into(), "订单.placed".into()],
            max_attempts: Some(5),
            first_delay_seconds: None,
            max_delay_seconds: Some(u32::MAX),
        }),
        Entry::RunCreated {
            run,
            sequence: u64::MAX,
            function: "charge".into(),
            code: "v1".into(),
            input: b"null".to_vec(),
        },
        Entry::Claimed {
            run,
            execution: 3,
            lease_until: Timestamp::from_millis(-1),
        },
        Entry::StepBegan {
            run,
            position: 1,
            name: "réserve".into(),
            once: true,
            input: Some(b"{}".to_vec()),
            execution: 3,
        },
        Entry::StepBegan {
            run,
            position: 2,
            name: "pay".into(),
            once: false,
            input: None,
            execution: 3,
        },
        Entry::StepCompleted {
            run,
            position: 1,
            output: b"{\"id\":  9007199254740993}".to_vec(),
        },
    ];
    let mut stored: Vec<_> = entries.into_iter().map(Into::into).collect();
    stored.push(completed);
    stored
}

#[test]
fn records_round_trip() {
    let env = new_env_ref();
    let record = ironflowx_events::Record {
        time: Timestamp::from_millis(-5),
        events: vec![ironflowx_events::NewEvent {
            sequence: 7,
            id: "019a8f3c-7e21-7b4a-9c3d-5e6f7a8b9c0d".parse().unwrap(),
            name: "订单.placed".into(),
            data: b"{\"a\":1}".to_vec(),
            metadata: Some(b"{}".to_vec()),
            version: 3,
            idempotency_key: Some("k".into()),
        }],
    };
    let key: Emitter = "key:019a9f3e-7c2b-7d41-8a3e-5f6b7c8d9e0f".parse().unwrap();
    let id = uuid::Uuid::now_v7();
    for emitted_by in [emitter(), key] {
        assert_eq!(
            record::decode(&events(env.token(), id, &emitted_by, &record)),
            Some(record::Decoded::Entries {
                format: record::FORMAT,
                token: env.token(),
                id,
                emitted_by,
                entries: Entries {
                    record: record.clone(),
                    execution: Vec::new(),
                },
            })
        );
    }
    assert_eq!(
        record::decode(&record::genesis(env)),
        Some(record::Decoded::Genesis {
            format: record::FORMAT,
            env
        })
    );
    let mut trailing = record::genesis(env);
    trailing.push(0);
    assert_eq!(record::decode(&trailing), None);
    // Execution entries of every kind, after the events, in their order.
    let run: ironflowx_core::RunId = "019a8f3c-7e21-7b4a-9c3d-5e6f7a8b9c0e".parse().unwrap();
    let execution = execution_entries(run);
    let entries = Entries {
        record: record.clone(),
        execution: execution.clone(),
    };
    let decoded = record::decode(&record::entries(env.token(), id, &emitter(), &entries));
    let Some(record::Decoded::Entries { entries: back, .. }) = decoded else {
        panic!("a record of entries decodes: {decoded:?}");
    };
    assert_eq!(back, entries);
    // An entry of a kind this format does not write: its kind follows the envelope and count.
    // Kinds 1 to 6 are execution entries; 7 is the first that is none.
    let mut unknown = events(env.token(), id, &emitter(), &record);
    let empty = ironflowx_events::Record {
        events: vec![],
        ..record
    };
    let kind = events(env.token(), id, &emitter(), &empty).len();
    for byte in [7, 0xFF] {
        unknown[kind] = byte;
        assert_eq!(record::decode(&unknown), None, "kind {byte}");
    }
    // A record ID that is not a UUIDv7 in its canonical text form, put in place of a good one.
    let good = events(env.token(), id, &emitter(), &empty);
    let at = good
        .windows(36)
        .position(|w| w == id.to_string().as_bytes());
    let at = at.unwrap();
    for bad in [
        "00000000-0000-0000-0000-000000000000",
        "019a8f3c-7e21-4b4a-9c3d-5e6f7a8b9c0d",
        "019a8f3c-7e21-7b4a-cc3d-5e6f7a8b9c0d",
        "019A8F3C-7E21-7B4A-9C3D-5E6F7A8B9C0D",
    ] {
        let mut bytes = good.clone();
        bytes[at..at + 36].copy_from_slice(bad.as_bytes());
        assert_eq!(record::decode(&bytes), None, "{bad}");
    }
    // The other text forms of the same ID, each with its own length prefix.
    let forms = [
        id.simple().to_string(),
        id.braced().to_string(),
        id.urn().to_string(),
    ];
    for form in forms {
        let mut bytes = good[..at - 4].to_vec();
        bytes.extend_from_slice(&u32::try_from(form.len()).unwrap().to_le_bytes());
        bytes.extend_from_slice(form.as_bytes());
        bytes.extend_from_slice(&good[at + 36..]);
        assert_eq!(record::decode(&bytes), None, "{form}");
    }
}

fn append(log: &SegmentLog, expected: ExpectedRevision, payload: Vec<u8>) {
    log.append(Submit {
        records: vec![NewRecord {
            stream: b"events".to_vec(),
            expected,
            key: None,
            payload,
        }],
    })
    .unwrap()
    .wait()
    .unwrap();
}

/// A ledger at `dir` holding events "a" and "b", closed; the ID of "a".
async fn two_events(dir: &Path, env: EnvRef) -> EventId {
    let ledger = open(dir, env).await;
    let id = emit(&ledger, keyed("a", "ka")).await.unwrap();
    emit(&ledger, keyed("b", "kb")).await.unwrap();
    ledger.close().await;
    id
}

/// What a test does to `read.db`, and why.
type Damage = (&'static str, fn(&Path));

/// The Open table's rebuild rows: each damage to `read.db` alone is rebuilt from the log.
#[tokio::test]
async fn a_read_store_that_cannot_be_trusted_is_rebuilt() {
    let damages: [Damage; 3] = [
        ("missing", |db| std::fs::remove_file(db).unwrap()),
        ("without meta", |db| {
            let conn = rusqlite::Connection::open(db).unwrap();
            conn.execute_batch("DROP TABLE meta").unwrap();
        }),
        ("unreadable", |db| {
            std::fs::write(db, b"not a database").unwrap()
        }),
    ];
    for (why, damage) in damages {
        let dir = tempfile::tempdir().unwrap();
        let env = new_env_ref();
        let id = two_events(dir.path(), env).await;
        damage(&dir.path().join("read.db"));
        let ledger = open(dir.path(), env).await;
        assert_eq!(names(&ledger).await, ["b", "a"], "{why}");
        let retry = outcome(&ledger, keyed("x", "ka")).await.unwrap();
        assert_eq!(retry, ItemOutcome::Duplicate(id), "{why}");
        // Sequences continue after the rebuilt rows.
        emit(&ledger, named("c")).await.unwrap();
        assert_eq!(names(&ledger).await, ["c", "b", "a"], "{why}");
    }
}

/// The read store proves the log lost durable records: refused, not rebuilt over.
#[tokio::test]
async fn a_log_behind_its_read_store_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let env = new_env_ref();
    two_events(dir.path(), env).await;
    let conn = rusqlite::Connection::open(dir.path().join("read.db")).unwrap();
    conn.execute_batch("UPDATE meta SET applied = 999").unwrap();
    drop(conn);
    let err = Ledger::open(dir.path(), env, clock()).await.err();
    assert!(matches!(err, Some(OpenError::Corrupt)), "{err:?}");
}

/// A bad byte in an acknowledged record of the unsealed newest segment, after a clean close: a
/// later record, or the close frame the close wrote, proves the record was acknowledged. The log
/// itself refuses, so open answers `Corrupt` without the read store, changes no file, and keeps
/// refusing.
#[tokio::test]
async fn damage_to_an_acknowledged_record_is_refused_without_its_read_store() {
    for (why, last) in [
        ("the first record, later records after it", false),
        ("the last record, only the close frame after it", true),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let env = new_env_ref();
        two_events(dir.path(), env).await;
        std::fs::remove_file(dir.path().join("read.db")).unwrap();
        let path = segment(dir.path());
        let mut bytes = std::fs::read(&path).unwrap();
        assert_eq!(
            bytes[bytes.len() - 24],
            7,
            "{why}: the segment ends with a close frame"
        );
        let at = if last {
            bytes.len() - CLOSE_FRAME - 1
        } else {
            64 + 40
        };
        bytes[at] ^= 0xff;
        std::fs::write(&path, &bytes).unwrap();
        let files = listing(dir.path());
        for _ in 0..2 {
            let err = Ledger::open(dir.path(), env, clock()).await.err();
            assert!(matches!(err, Some(OpenError::Corrupt)), "{why}: {err:?}");
            // The log itself refuses, not only the ledger (segment-log ADRs 0068 and 0071).
            let err = SegmentLog::open(dir.path().join("log"), Options::default()).err();
            assert!(
                matches!(err, Some(segment_log_core::OpenError::Corrupt { .. })),
                "{why}: {err:?}"
            );
        }
        assert_eq!(std::fs::read(&path).unwrap(), bytes, "{why}");
        assert_eq!(listing(dir.path()), files, "{why}");
    }
}

/// A clean close writes a close frame of this many bytes after its last record (segment-log
/// format version 4).
const CLOSE_FRAME: usize = 32;

/// A crash writes no close frame, so a bad last record has nothing after it: the log cannot tell
/// it from a torn write and truncates it. Only `read.db` remembers it was acknowledged: with it,
/// open refuses; without it, open recovers the records before it and the last one is lost
/// unreported.
#[tokio::test]
async fn after_a_crash_a_bad_last_record_is_truncated_and_only_the_read_store_notices() {
    let damages: [Damage; 2] = [
        ("a flipped byte in the last frame", |path| {
            let mut bytes = std::fs::read(path).unwrap();
            *bytes.last_mut().unwrap() ^= 0xff;
            std::fs::write(path, bytes).unwrap();
        }),
        ("a torn write: the last frame cut short", |path| {
            cut(path, 5)
        }),
    ];
    for (why, damage) in damages {
        let dir = tempfile::tempdir().unwrap();
        let env = new_env_ref();
        two_events(dir.path(), env).await;
        let path = segment(dir.path());
        // What a crash leaves: the same bytes, without the close frame.
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(
            bytes[bytes.len() - 24],
            7,
            "{why}: the segment ends with a close frame"
        );
        cut(&path, CLOSE_FRAME as u64);
        damage(&path);
        let err = Ledger::open(dir.path(), env, clock()).await.err();
        assert!(matches!(err, Some(OpenError::Corrupt)), "{why}: {err:?}");
        std::fs::remove_file(dir.path().join("read.db")).unwrap();
        let ledger = open(dir.path(), env).await;
        assert_eq!(names(&ledger).await, ["a"], "{why}");
    }
}

/// Cuts the last `n` bytes off the file at `path`.
fn cut(path: &Path, n: u64) {
    let len = std::fs::metadata(path).unwrap().len();
    std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap()
        .set_len(len - n)
        .unwrap();
}

/// Every path under `dir`, relative to it.
fn listing(dir: &Path) -> BTreeSet<std::path::PathBuf> {
    let mut out = BTreeSet::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path.clone());
            }
            out.insert(path.strip_prefix(dir).unwrap().to_path_buf());
        }
    }
    out
}

fn segment(dir: &Path) -> std::path::PathBuf {
    dir.join("log").join(format!("{:020}.seg", 0))
}

#[tokio::test]
async fn a_corrupt_log_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let env = new_env_ref();
    two_events(dir.path(), env).await;
    // Covers a sealed segment; `damage_to_an_acknowledged_record_is_refused_without_its_read_store`
    // covers an unsealed one.
    SegmentLog::open(dir.path().join("log"), Options::default())
        .unwrap()
        .seal()
        .unwrap();
    // One byte inside the first record's frame, with later frames intact behind it.
    let path = segment(dir.path());
    let mut bytes = std::fs::read(&path).unwrap();
    bytes[64 + 40] ^= 0xff;
    std::fs::write(&path, bytes).unwrap();
    let err = Ledger::open(dir.path(), env, clock()).await.err();
    assert!(matches!(err, Some(OpenError::Corrupt)), "{err:?}");
}

#[tokio::test]
async fn a_log_in_an_unsupported_format_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let env = new_env_ref();
    two_events(dir.path(), env).await;
    // The segment header's format version (bytes 4..6), under a valid header checksum.
    let path = segment(dir.path());
    let mut bytes = std::fs::read(&path).unwrap();
    bytes[4..6].copy_from_slice(&999u16.to_le_bytes());
    let crc = crc32c(&bytes[..60]);
    bytes[60..64].copy_from_slice(&crc.to_le_bytes());
    std::fs::write(&path, bytes).unwrap();
    let err = Ledger::open(dir.path(), env, clock()).await.err();
    assert!(matches!(err, Some(OpenError::Format)), "{err:?}");
}

/// CRC-32C (Castagnoli), bitwise: the segment header's checksum.
fn crc32c(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in bytes {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0x82f6_3b78
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// D19: a sealed log opens and reads; the first emit is a fault.
#[tokio::test]
async fn a_sealed_log_opens_and_reads_and_refuses_the_first_emit() {
    let dir = tempfile::tempdir().unwrap();
    let env = new_env_ref();
    two_events(dir.path(), env).await;
    SegmentLog::open(dir.path().join("log"), Options::default())
        .unwrap()
        .seal()
        .unwrap();
    let ledger = open(dir.path(), env).await;
    assert_eq!(names(&ledger).await, ["b", "a"]);
    assert!(matches!(
        Ledger::open(dir.path(), env, clock()).await,
        Err(OpenError::Locked)
    ));
    let err = emit(&ledger, named("c")).await.unwrap_err();
    assert_eq!(err, SubmitError::Unavailable);
    assert_eq!(all(&ledger).await.unwrap_err(), ReadError::Unavailable);
}

/// A submission still queued when the ledger is dropped was never decided: it is not stored.
#[tokio::test]
async fn a_submission_queued_at_drop_is_not_stored() {
    let dir = tempfile::tempdir().unwrap();
    let env = new_env_ref();
    let ledger = open(dir.path(), env).await;
    ledger.hold_fold();
    emit(&ledger, named("a")).await.unwrap();
    {
        // Queued behind the held fold; its caller gives up waiting.
        let queued = emit(&ledger, named("queued"));
        tokio::pin!(queued);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut queued)
                .await
                .is_err()
        );
    }
    drop(ledger);
    let ledger = open(dir.path(), env).await;
    assert_eq!(names(&ledger).await, ["a"]);
}

/// Close, unlike drop, finishes every queued submission.
#[tokio::test]
async fn close_finishes_queued_submissions() {
    let dir = tempfile::tempdir().unwrap();
    let env = new_env_ref();
    let ledger = open(dir.path(), env).await;
    ledger.hold_fold();
    emit(&ledger, named("a")).await.unwrap();
    // The submission enters the queue in its first poll and cannot complete while fold is held.
    abandon(emit(&ledger, named("queued"))).await;
    assert_eq!(ledger.close_with_state().await, LedgerState::Open);
    let ledger = open(dir.path(), env).await;
    assert_eq!(names(&ledger).await, ["queued", "a"]);
}

#[tokio::test]
async fn a_divergent_read_store_of_the_same_incarnation_is_rebuilt() {
    let dir = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let env = new_env_ref();
    for (path, name) in [(dir.path(), "mine"), (other.path(), "theirs")] {
        let ledger = open(path, env).await;
        emit(&ledger, keyed(name, "key")).await.unwrap();
        ledger.close().await;
    }
    std::fs::copy(other.path().join("read.db"), dir.path().join("read.db")).unwrap();
    let ledger = open(dir.path(), env).await;
    assert_eq!(names(&ledger).await, ["mine"]);
}

/// Every record of the closed ledger at `dir`, decoded, and its log's watermark.
fn log_records(dir: &Path) -> (Vec<record::Decoded>, u64) {
    let log = SegmentLog::open(dir.join("log"), Options::default()).unwrap();
    let records = log.read(0, usize::MAX).unwrap();
    let decoded = records.iter().map(|r| record::decode(&r.payload).unwrap());
    (decoded.collect(), log.watermark())
}

/// The IDs of the records after the genesis record, in log order.
fn record_ids(dir: &Path) -> Vec<uuid::Uuid> {
    let (records, _) = log_records(dir);
    let id = |r| match r {
        record::Decoded::Entries { id, .. } => Some(id),
        record::Decoded::Genesis { .. } => None,
    };
    records.into_iter().filter_map(id).collect()
}

/// Copies every file under `from` into the empty directory `to`.
fn copy_dir(from: &Path, to: &Path) {
    for path in listing(from) {
        if from.join(&path).is_dir() {
            std::fs::create_dir_all(to.join(&path)).unwrap();
        } else {
            std::fs::create_dir_all(to.join(&path).parent().unwrap()).unwrap();
            std::fs::copy(from.join(&path), to.join(&path)).unwrap();
        }
    }
}

/// Appends a record that holds no event to the closed ledger at `dir`, at time `T`.
fn append_eventless(dir: &Path, env: EnvRef) {
    let log = SegmentLog::open(dir.join("log"), Options::default()).unwrap();
    let revision = log.stream_head(b"events").unwrap().revision;
    let record = ironflowx_events::Record {
        time: Timestamp::from_millis(T),
        events: vec![],
    };
    let payload = events(env.token(), uuid::Uuid::now_v7(), &emitter(), &record);
    append(&log, ExpectedRevision::At(revision), payload);
}

#[tokio::test]
async fn a_fresh_ledger_writes_format_3() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = open(dir.path(), new_env_ref()).await;
    emit(&ledger, named("a")).await.unwrap();
    ledger.close().await;
    let format = |r: &record::Decoded| match *r {
        record::Decoded::Genesis { format, .. } | record::Decoded::Entries { format, .. } => format,
    };
    let (records, _) = log_records(dir.path());
    assert_eq!(records.iter().map(format).collect::<Vec<_>>(), [3, 3]);
}

/// The writer draws each record's ID at the append: a copy keeps the IDs it copied, and every
/// record each copy appends next has its own (D117). That the writer draws only UUIDv7s is
/// pinned by the decoder, which refuses any other ID at the reopen.
#[tokio::test]
async fn records_appended_to_two_copies_of_a_directory_have_their_own_ids() {
    let dir = tempfile::tempdir().unwrap();
    let copy = tempfile::tempdir().unwrap();
    let env = new_env_ref();
    let ledger = open(dir.path(), env).await;
    emit(&ledger, named("a")).await.unwrap();
    ledger.close().await;
    copy_dir(dir.path(), copy.path());
    for path in [dir.path(), copy.path()] {
        let ledger = open(path, env).await;
        emit(&ledger, named("b")).await.unwrap();
        emit(&ledger, named("c")).await.unwrap();
        ledger.close().await;
    }
    let (mine, theirs) = (record_ids(dir.path()), record_ids(copy.path()));
    assert_eq!((mine.len(), theirs.len()), (3, 3));
    assert_eq!(mine[0], theirs[0], "the copied record keeps its ID");
    let all: BTreeSet<_> = mine.iter().chain(&theirs[1..]).collect();
    assert_eq!(all.len(), 5, "{mine:?} {theirs:?}");
}

/// Open compares the last applied record's ID with the log's record at that position (D117,
/// LED-8 and LED-12 extended): a read store of a copy that diverged after its last event, by a
/// record of the same kind at the same position and time, holding no event, is rebuilt. The
/// open appends nothing, and the rebuilt store reopens as it is.
#[tokio::test]
async fn a_read_store_of_a_copy_diverged_by_a_record_without_events_is_rebuilt() {
    let dir = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let env = new_env_ref();
    let ledger = open(dir.path(), env).await;
    emit(&ledger, named("a")).await.unwrap();
    ledger.close().await;
    copy_dir(dir.path(), other.path());
    for path in [dir.path(), other.path()] {
        append_eventless(path, env);
        open(path, env).await.close().await;
    }
    std::fs::copy(other.path().join("read.db"), dir.path().join("read.db")).unwrap();
    let ours = record_ids(dir.path());
    assert_ne!(ours[1], record_ids(other.path())[1]);
    let (_, watermark) = log_records(dir.path());
    let db = dir.path().join("read.db");
    for open_ in ["rebuilds", "reopens"] {
        let ledger = open(dir.path(), env).await;
        assert_eq!(names(&ledger).await, ["a"], "{open_}");
        let last = crate::store::last_record(&ledger.store.lock().unwrap()).unwrap();
        assert_eq!(last, Some(ours[1].to_string()), "{open_}");
        ledger.close().await;
        assert_eq!(
            log_records(dir.path()).1,
            watermark,
            "{open_}: open appended"
        );
        // A rebuild would drop a table the fold knows nothing of: the reopen keeps it.
        let conn = rusqlite::Connection::open(&db).unwrap();
        let sentinel = "SELECT count(*) FROM sqlite_master WHERE name = 'sentinel'";
        let kept: u64 = conn.query_row(sentinel, [], |r| r.get(0)).unwrap();
        assert_eq!(kept, u64::from(open_ == "reopens"), "{open_}");
        conn.execute_batch("CREATE TABLE IF NOT EXISTS sentinel (x)")
            .unwrap();
    }
}

/// A read store whose record ID matches the log's record at its applied position is kept, a
/// matching store being one the writer's own fold left; one without an ID there is rebuilt (D117).
#[tokio::test]
async fn a_read_store_is_kept_only_when_its_record_id_matches() {
    for (damage, kept) in [
        ("", true),
        ("UPDATE meta SET record_id = NULL", false),
        // The right ID in another text form: the fold stores only the canonical one.
        ("UPDATE meta SET record_id = upper(record_id)", false),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let env = new_env_ref();
        let ledger = open(dir.path(), env).await;
        emit(&ledger, named("a")).await.unwrap();
        ledger.close().await;
        let db = dir.path().join("read.db");
        let conn = rusqlite::Connection::open(&db).unwrap();
        // A row the log does not hold: a rebuild would put "a" back.
        conn.execute_batch(&format!("UPDATE events SET name = 'kept'; {damage}"))
            .unwrap();
        drop(conn);
        // The log's end is past the applied position: open compares at the position.
        append_eventless(dir.path(), env);
        let ledger = open(dir.path(), env).await;
        let expected = if kept { "kept" } else { "a" };
        assert_eq!(names(&ledger).await, [expected], "{damage}");
        ledger.close().await;
        let conn = rusqlite::Connection::open(&db).unwrap();
        let sql = "SELECT applied, record_id FROM meta";
        let meta: (u64, Option<String>) = conn
            .query_row(sql, [], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        let id = record_ids(dir.path())[1].to_string();
        assert_eq!(meta, (3, Some(id)), "{damage}");
    }
}

/// A read store that holds only the genesis record has no record ID; one that says otherwise is
/// rebuilt (D117).
#[tokio::test]
async fn a_read_store_of_only_the_genesis_record_has_no_record_id() {
    let dir = tempfile::tempdir().unwrap();
    let env = new_env_ref();
    open(dir.path(), env).await.close().await;
    let db = dir.path().join("read.db");
    let meta = || {
        let conn = rusqlite::Connection::open(&db).unwrap();
        let sql = "SELECT applied, record_id FROM meta";
        let row = conn.query_row(sql, [], |r| Ok((r.get(0)?, r.get(1)?)));
        row.unwrap()
    };
    assert_eq!(meta(), (1u64, None::<String>));
    for id in [uuid::Uuid::now_v7().to_string(), "not a record ID".into()] {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute("UPDATE meta SET record_id = ?1", [&id])
            .unwrap();
        drop(conn);
        open(dir.path(), env).await.close().await;
        assert_eq!(meta(), (1, None), "{id}");
    }
}

#[tokio::test]
async fn a_missing_log_preserves_a_populated_foreign_store() {
    let dir = tempfile::tempdir().unwrap();
    let env = new_env_ref();
    two_events(dir.path(), env).await;
    std::fs::rename(dir.path().join("log"), dir.path().join("backup")).unwrap();
    assert!(matches!(
        Ledger::open(dir.path(), new_env_ref(), clock()).await,
        Err(OpenError::Corrupt)
    ));
    let conn = rusqlite::Connection::open(dir.path().join("read.db")).unwrap();
    assert_eq!(crate::store::applied(&conn).unwrap(), 3);
}

#[tokio::test]
async fn abandoned_waiters_do_not_fill_an_unbounded_writer_queue() {
    let dir = tempfile::tempdir().unwrap();
    let env = new_env_ref();
    let ledger = open(dir.path(), env).await;
    ledger.hold_fold();
    emit(&ledger, named("first")).await.unwrap();
    // Poll each submit once and cancel before releasing the writer.
    for _ in 0..17 {
        tokio::select! {
            biased;
            result = emit(&ledger, named("queued")) => panic!("unexpected reply {result:?}"),
            () = std::future::ready(()) => {}
        }
    }
    ledger.close().await;
    let ledger = open(dir.path(), env).await;
    assert_eq!(all(&ledger).await.unwrap().len(), 17); // first plus 16 admitted
}

#[tokio::test]
async fn a_writer_panic_before_append_refuses_queued_work() {
    struct Panics(std::sync::mpsc::Receiver<()>);
    impl ironflowx_core::Clock for Panics {
        fn now(&self) -> Timestamp {
            self.0.recv().unwrap();
            panic!("clock failed");
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let (release, wait) = std::sync::mpsc::channel();
    let ledger = Ledger::open(dir.path(), new_env_ref(), Panics(wait))
        .await
        .unwrap();
    let first = emit(&ledger, named("first"));
    let second = emit(&ledger, named("queued"));
    tokio::pin!(first, second);
    tokio::select! {
        biased;
        r = &mut first => panic!("{r:?}"),
        r = &mut second => panic!("{r:?}"),
        () = std::future::ready(()) => {}
    }
    release.send(()).unwrap();
    assert_eq!(first.await.unwrap_err(), SubmitError::Unavailable);
    assert_eq!(ledger.state(), LedgerState::Unavailable);
    assert_eq!(second.await.unwrap_err(), SubmitError::Unavailable);
}

#[tokio::test]
async fn an_empty_log_preserves_a_store_with_unreadable_progress() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("read.db");
    std::fs::write(&db, b"damaged database").unwrap();
    let result = Ledger::open(dir.path(), new_env_ref(), clock()).await;
    assert!(matches!(result, Err(OpenError::Io(_))));
    assert_eq!(std::fs::read(db).unwrap(), b"damaged database");
}

#[tokio::test]
async fn every_event_records_who_emitted_it_across_reopen_and_rebuild() {
    let dir = tempfile::tempdir().unwrap();
    let env = new_env_ref();
    let ledger = open(dir.path(), env).await;
    let batch = Submission::Events(Events::Batch(["a", "b", "c"].map(named).into()));
    let app = SubmitAs::embedder("billing app").unwrap();
    let committed = ledger.submit(batch, app.clone()).await.unwrap();
    let solo = emit(&ledger, named("d")).await.unwrap();
    let expected = [
        emitter(),
        Emitter::from(app.clone()),
        Emitter::from(app.clone()),
        Emitter::from(app),
    ];
    let emitters = async |ledger: &Ledger| -> Vec<Emitter> {
        all(ledger)
            .await
            .unwrap()
            .into_iter()
            .map(|e| e.emitted_by)
            .collect()
    };
    assert_eq!(emitters(&ledger).await, expected);
    let ItemOutcome::New(first) = items(committed.outcome)[0] else {
        panic!("a new event");
    };
    let read = get(&ledger, first).await.unwrap();
    assert_eq!(read.emitted_by.to_string(), "embedder:billing app");
    assert_eq!(get(&ledger, solo).await.unwrap().emitted_by, emitter());
    ledger.close().await;
    let ledger = open(dir.path(), env).await;
    assert_eq!(emitters(&ledger).await, expected);
    ledger.close().await;
    std::fs::remove_file(dir.path().join("read.db")).unwrap();
    let ledger = open(dir.path(), env).await;
    assert_eq!(emitters(&ledger).await, expected);
}

/// A directory from 0.1 (format 1) or 0.2–0.3 (format 2) is refused, not misread (D8).
#[tokio::test]
async fn an_older_format_directory_is_refused_as_another_format() {
    for format in [1u16, 2] {
        let dir = tempfile::tempdir().unwrap();
        let env = new_env_ref();
        {
            let log = SegmentLog::open(dir.path().join("log"), Options::default()).unwrap();
            let mut genesis = record::genesis(env);
            genesis[1..3].copy_from_slice(&format.to_le_bytes());
            append(&log, ExpectedRevision::Empty, genesis);
        }
        let err = Ledger::open(dir.path(), env, clock()).await.err();
        assert!(
            matches!(err, Some(OpenError::Format)),
            "format {format}: {err:?}"
        );
        assert_eq!(
            err.unwrap().to_string(),
            "the ledger directory is in another format",
            "format {format}"
        );
    }
}

/// Every file under `dir` with its bytes, and every directory.
fn contents(dir: &Path) -> Vec<(std::path::PathBuf, Option<Vec<u8>>)> {
    let read = |p: &Path| p.is_file().then(|| std::fs::read(p).unwrap());
    let whole = |p: std::path::PathBuf| (p.clone(), read(&dir.join(&p)));
    listing(dir).into_iter().map(whole).collect()
}

/// Opening an existing ledger refuses a directory that holds none, whatever else it holds, and
/// creates nothing in it: the records are lost, and a fresh ledger would hide that (TEN-12).
#[tokio::test]
async fn opening_an_existing_ledger_refuses_a_missing_one_and_changes_nothing() {
    type Loss = (&'static str, fn(&Path));
    let losses: [Loss; 7] = [
        ("the directory", |d| std::fs::remove_dir_all(d).unwrap()),
        ("its contents", |d| {
            std::fs::remove_dir_all(d).unwrap();
            std::fs::create_dir(d).unwrap();
        }),
        ("the log, the read store kept", |d| {
            std::fs::remove_dir_all(d.join("log")).unwrap();
        }),
        ("the log's contents", |d| {
            std::fs::remove_dir_all(d.join("log")).unwrap();
            std::fs::create_dir(d.join("log")).unwrap();
        }),
        ("the log's segments", |d| {
            std::fs::remove_file(segment(d)).unwrap();
        }),
        ("the log's segments, files the log ignores left", |d| {
            std::fs::remove_file(segment(d)).unwrap();
            let names = [
                "notes.seg",
                "0.seg",
                "0000000000000000000a.seg",
                "00000000000000000000.bak",
            ];
            for name in names {
                std::fs::write(d.join("log").join(name), "").unwrap();
            }
        }),
        ("every record, the genesis record too", |d| {
            std::fs::remove_dir_all(d.join("log")).unwrap();
            drop(SegmentLog::open(d.join("log"), Options::default()).unwrap());
        }),
    ];
    for (lost, lose) in losses {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("ledger");
        let env = new_env_ref();
        two_events(&dir, env).await;
        lose(&dir);
        let before = contents(root.path());
        let err = Ledger::open_existing_blocking(&dir, env, clock()).err();
        assert!(matches!(err, Some(OpenError::Missing)), "{lost}: {err:?}");
        assert_eq!(contents(root.path()), before, "{lost}");
    }
}

/// An existing ledger opens with its records, and is refused as `open` refuses it.
#[tokio::test]
async fn opening_an_existing_ledger_opens_or_refuses_it_as_open_does() {
    let dir = tempfile::tempdir().unwrap();
    let env = new_env_ref();
    two_events(dir.path(), env).await;
    let ledger = Ledger::open_existing_blocking(dir.path(), env, clock()).unwrap();
    assert_eq!(names(&ledger).await, ["b", "a"]);
    let err = Ledger::open_existing_blocking(dir.path(), env, clock()).err();
    assert!(matches!(err, Some(OpenError::Locked)), "{err:?}");
    ledger.close().await;
    let before = contents(dir.path());
    let err = Ledger::open_existing_blocking(dir.path(), new_env_ref(), clock()).err();
    assert!(matches!(err, Some(OpenError::WrongIncarnation)), "{err:?}");
    assert_eq!(contents(dir.path()), before, "another incarnation");
    std::fs::remove_file(dir.path().join("read.db")).unwrap();
    let ledger = Ledger::open_existing_blocking(dir.path(), env, clock()).unwrap();
    assert_eq!(names(&ledger).await, ["b", "a"]);
}

/// Whether `f` is ready when polled once.
fn ready<F: Future>(f: std::pin::Pin<&mut F>) -> bool {
    f.poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
        .is_ready()
}

/// Starts `call`, then gives up waiting for it.
async fn abandon<T: std::fmt::Debug>(call: impl Future<Output = T>) {
    tokio::select! {
        biased;
        r = call => panic!("answered {r:?}"),
        () = std::future::ready(()) => {}
    }
}

/// Reads `T` each time the test sends `false`; panics when it sends `true`.
struct Stepped(std::sync::mpsc::Receiver<bool>);

impl ironflowx_core::Clock for Stepped {
    fn now(&self) -> Timestamp {
        assert!(!self.0.recv().unwrap(), "the clock panics");
        Timestamp::from_millis(T)
    }
}

/// A way a ledger fails: its failing call, held until the test has arranged the failure.
struct Way {
    name: &'static str,
    /// The failing call is a read, whose query is held; else an emit, whose decision is.
    reads: bool,
    /// The query, or the clock the decision reads, panics.
    panics: bool,
    /// The log is sealed before the ledger opens.
    sealed: bool,
    /// Run on `read.db`.
    sql: &'static str,
    arrange: fn(&Ledger),
}

const WAY: Way = Way {
    name: "",
    reads: false,
    panics: false,
    sealed: false,
    sql: "",
    arrange: |_| {},
};

const WAYS: [Way; 9] = [
    Way {
        name: "append lost",
        arrange: |l| l.fail_next_append(false),
        ..WAY
    },
    Way {
        name: "append landed",
        arrange: |l| l.fail_next_append(true),
        ..WAY
    },
    Way {
        name: "sealed log",
        sealed: true,
        ..WAY
    },
    Way {
        name: "fold",
        arrange: Ledger::fail_next_fold,
        ..WAY
    },
    Way {
        name: "fold in the read store",
        sql: "CREATE TRIGGER t BEFORE INSERT ON events BEGIN SELECT RAISE(ABORT, 'x'); END",
        ..WAY
    },
    Way {
        name: "emit in the read store",
        sql: "DROP TABLE events",
        ..WAY
    },
    Way {
        name: "read in the read store",
        reads: true,
        sql: "DROP TABLE events",
        ..WAY
    },
    Way {
        name: "panicking query",
        reads: true,
        panics: true,
        ..WAY
    },
    Way {
        name: "writer panic",
        panics: true,
        ..WAY
    },
];

/// Each way a ledger fails, the failing call started and then abandoned, so nobody waits for an
/// answer when it happens: the fault is signalled all the same, and stays.
#[tokio::test]
async fn each_failure_is_signalled_with_no_call_in_flight() {
    for way in WAYS {
        let name = way.name;
        let dir = tempfile::tempdir().unwrap();
        let env = new_env_ref();
        open(dir.path(), env).await.close().await;
        if way.sealed {
            SegmentLog::open(dir.path().join("log"), Options::default())
                .unwrap()
                .seal()
                .unwrap();
        }
        let (step, steps) = std::sync::mpsc::channel();
        let ledger = Ledger::open(dir.path(), env, Stepped(steps)).await.unwrap();
        let mut faulted = std::pin::pin!(ledger.faulted());
        assert!(!ready(faulted.as_mut()), "{name}: open");
        assert_eq!(ledger.state(), LedgerState::Open, "{name}");

        let (go, gone) = std::sync::mpsc::channel::<()>();
        if way.reads {
            let read = ledger.read(move |v| {
                gone.recv().unwrap();
                assert!(!way.panics, "a query bug");
                ironflowx_events::list_events(v, env.token(), None, None)
            });
            abandon(read).await;
        } else {
            abandon(emit(&ledger, named("a"))).await;
        }
        let db = rusqlite::Connection::open(dir.path().join("read.db")).unwrap();
        db.execute_batch(way.sql).unwrap();
        (way.arrange)(&ledger);
        assert!(!ready(faulted.as_mut()), "{name}: not yet");
        if way.reads {
            go.send(()).unwrap();
        } else {
            step.send(way.panics).unwrap();
        }

        let signalled = tokio::time::timeout(Duration::from_secs(10), faulted).await;
        assert!(signalled.is_ok(), "{name}: not signalled");
        assert_eq!(ledger.state(), LedgerState::Unavailable, "{name}");
        // At once, also to a task that has spent its budget.
        while tokio::task::coop::has_budget_remaining() {
            tokio::task::consume_budget().await;
        }
        let later = std::pin::pin!(ledger.faulted());
        assert!(ready(later), "{name}: taken after the fault");
        // Not settled by what failed: a later call is refused too.
        let refused = emit(&ledger, named("b")).await.unwrap_err();
        assert_eq!(refused, SubmitError::Unavailable, "{name}");
        ledger.close().await;
    }
}

/// A ledger that answers is not faulted, whatever it refuses, and neither its close nor its drop is
/// a fault: what `faulted` gave before stays pending.
#[tokio::test]
async fn neither_a_refusal_nor_a_close_is_a_fault() {
    for close in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let ledger = open(dir.path(), new_env_ref()).await;
        let mut faulted = std::pin::pin!(ledger.faulted());
        emit(&ledger, keyed("a", "k")).await.unwrap();
        emit(&ledger, keyed("a", "k")).await.unwrap();
        let refused = emit(&ledger, named("a..b")).await;
        let Err(SubmitError::Refused(r)) = refused else {
            panic!("{refused:?}");
        };
        let name = (ErrorKind::Invalid, "event name has an empty segment");
        assert_eq!((r.kind(), r.message()), name);
        let token = ledger.env().token();
        let refused = ledger
            .read(move |v| ironflowx_events::list_events(v, token, None, Some("bad")))
            .await;
        let Err(ReadError::Refused(r)) = refused else {
            panic!("{refused:?}");
        };
        let cursor = (ErrorKind::Invalid, "invalid cursor");
        assert_eq!((r.kind(), r.message()), cursor);
        assert!(!ready(faulted.as_mut()));
        assert_eq!(ledger.state(), LedgerState::Open);
        // Queued behind the held fold: a close commits it, a drop refuses it.
        ledger.hold_fold();
        emit(&ledger, named("b")).await.unwrap();
        abandon(emit(&ledger, named("queued"))).await;
        let mut unpolled = std::pin::pin!(ledger.faulted());
        if close {
            ledger.close().await;
        } else {
            drop(ledger);
        }
        assert!(!ready(faulted.as_mut()), "close: {close}");
        assert!(!ready(unpolled.as_mut()), "close: {close}");
    }
}

/// A fault while a close or a drop finishes the fold is one, and outlasts the ledger.
#[tokio::test]
async fn a_fault_during_a_close_or_drop_is_signalled() {
    for close in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let ledger = open(dir.path(), new_env_ref()).await;
        ledger.hold_fold();
        ledger.fail_next_fold();
        emit(&ledger, named("a")).await.unwrap();
        let mut faulted = std::pin::pin!(ledger.faulted());
        assert!(!ready(faulted.as_mut()));
        if close {
            assert_eq!(ledger.close_with_state().await, LedgerState::Open);
        } else {
            drop(ledger);
        }
        assert!(ready(faulted), "close: {close}");
    }
}

#[tokio::test]
async fn close_reports_a_fault_published_after_the_hosts_last_observation() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = open(dir.path(), new_env_ref()).await;
    ledger.hold_fold();
    ledger.fail_next_fold();
    emit(&ledger, named("a")).await.unwrap();
    let mut faulted = std::pin::pin!(ledger.faulted());
    assert!(!ready(faulted.as_mut()));
    ledger.release_fold();
    tokio::time::timeout(Duration::from_secs(10), faulted)
        .await
        .unwrap();
    assert_eq!(ledger.close_with_state().await, LedgerState::Unavailable);
}

/// Panics when it is dropped, as the writer ends.
struct PanicsOnDrop;

impl ironflowx_core::Clock for PanicsOnDrop {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(T)
    }
}

impl Drop for PanicsOnDrop {
    fn drop(&mut self) {
        panic!("the clock's drop panics");
    }
}

#[tokio::test]
async fn a_panic_as_the_writer_ends_is_signalled() {
    for close in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::open(dir.path(), new_env_ref(), PanicsOnDrop).await;
        let ledger = ledger.unwrap();
        let mut faulted = std::pin::pin!(ledger.faulted());
        assert!(!ready(faulted.as_mut()));
        if close {
            ledger.close().await;
        } else {
            drop(ledger);
        }
        assert!(ready(faulted), "close: {close}");
    }
}

/// Poisons the read store's lock without failing the ledger, as another thread may find it between
/// a query's panic and the fault: whoever finds it fails the ledger.
#[tokio::test]
async fn whoever_finds_the_read_store_poisoned_fails_the_ledger() {
    for finder in ["read", "decide", "fold"] {
        let dir = tempfile::tempdir().unwrap();
        let ledger = open(dir.path(), new_env_ref()).await;
        let faulted = ledger.faulted();
        ledger.hold_fold();
        if finder == "fold" {
            emit(&ledger, named("a")).await.unwrap();
        }
        let store = ledger.store.clone();
        let poison = std::thread::spawn(move || {
            let _held = store.lock().unwrap();
            panic!("a query bug");
        });
        poison.join().unwrap_err();
        assert_eq!(ledger.state(), LedgerState::Open, "{finder}");
        match finder {
            "read" => assert_eq!(all(&ledger).await.unwrap_err(), ReadError::Unavailable),
            "decide" => {
                let refused = emit(&ledger, named("a")).await.unwrap_err();
                assert_eq!(refused, SubmitError::Unavailable);
            }
            _ => ledger.release_fold(),
        }
        let signalled = tokio::time::timeout(Duration::from_secs(10), faulted).await;
        assert!(signalled.is_ok(), "{finder}: not signalled");
        ledger.close().await;
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
}

/// A query its caller abandoned can end after the ledger closed, which is no fault.
#[test]
fn a_query_that_fails_after_close_is_no_fault() {
    for panics in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let (go, gone) = std::sync::mpsc::channel::<()>();
        let (start, started) = std::sync::mpsc::channel::<()>();
        let rt = runtime();
        let ledger = rt.block_on(open(dir.path(), new_env_ref()));
        let mut faulted = std::pin::pin!(ledger.faulted());
        let token = ledger.env().token();
        rt.block_on(async {
            let read = ledger.read(move |v| {
                start.send(()).unwrap();
                gone.recv().unwrap();
                assert!(!panics, "a query bug");
                ironflowx_events::list_events(v, token, None, None)
            });
            abandon(read).await;
            started.recv().unwrap();
            let db = rusqlite::Connection::open(dir.path().join("read.db")).unwrap();
            db.execute_batch("DROP TABLE events").unwrap();
            ledger.close().await;
        });
        go.send(()).unwrap();
        // Waits for the query to end.
        drop(rt);
        assert!(!ready(faulted.as_mut()), "panics: {panics}");
    }
}

/// A read whose query never ran, its runtime shut down, answers as a failed ledger, so it fails it.
#[test]
fn a_read_on_a_runtime_that_shut_down_fails_the_ledger() {
    let dir = tempfile::tempdir().unwrap();
    let rt = runtime();
    let ledger = rt.block_on(open(dir.path(), new_env_ref()));
    let handle = rt.handle().clone();
    drop(rt);
    let read = handle.block_on(all(&ledger));
    assert_eq!(read.unwrap_err(), ReadError::Unavailable);
    assert_eq!(ledger.state(), LedgerState::Unavailable);
}

/// A close first polled on a runtime that has shut down drops the ledger, which joins the writer:
/// only a join finds the panic of its end.
#[test]
fn a_close_on_a_runtime_that_shut_down_joins_the_writer() {
    let dir = tempfile::tempdir().unwrap();
    let rt = runtime();
    let ledger = rt.block_on(Ledger::open(dir.path(), new_env_ref(), PanicsOnDrop));
    let ledger = ledger.unwrap();
    let faulted = std::pin::pin!(ledger.faulted());
    let handle = rt.handle().clone();
    drop(rt);
    let close = std::panic::AssertUnwindSafe(|| handle.block_on(ledger.close()));
    std::panic::catch_unwind(close).unwrap_err();
    assert!(ready(faulted));
}

/// Its drop, as the writer ends, ends the receiver's wait.
struct Telling(std::sync::mpsc::Sender<()>);

impl ironflowx_core::Clock for Telling {
    fn now(&self) -> Timestamp {
        let _ = self.0.send(());
        Timestamp::from_millis(T)
    }
}

/// A close ends the queue before its blocking work has a thread: the only one here is busy until
/// the writer has ended.
#[test]
fn a_close_ends_the_queue_without_a_blocking_thread() {
    let dir = tempfile::tempdir().unwrap();
    let (tell, told) = std::sync::mpsc::channel();
    let rt = tokio::runtime::Builder::new_current_thread()
        .max_blocking_threads(1)
        .enable_time()
        .build()
        .unwrap();
    let closed = rt.block_on(async {
        let ledger = Ledger::open(dir.path(), new_env_ref(), Telling(tell)).await;
        tokio::task::spawn_blocking(move || while told.recv().is_ok() {});
        tokio::time::timeout(Duration::from_secs(10), ledger.unwrap().close()).await
    });
    rt.shutdown_background();
    closed.expect("the close ended the queue");
}

#[tokio::test]
async fn the_ledger_gets_an_event_and_lists_pages_of_its_own() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = open(dir.path(), new_env_ref()).await;
    let a = emit(&ledger, named("a")).await.unwrap();
    emit(&ledger, named("b")).await.unwrap();
    let got = ledger.get_event(&a.to_string()).await.unwrap();
    assert_eq!((got.id, &*got.name), (a, "a"));
    let Err(ReadError::Refused(r)) = ledger.get_event("evt_unknown").await else {
        panic!("an unknown ID was found");
    };
    assert_eq!(
        (r.kind(), r.message()),
        (ErrorKind::NotFound, "event not found")
    );
    let first = ledger.list_events(1.try_into().ok(), None).await.unwrap();
    assert_eq!(
        first.events.iter().map(|e| &*e.name).collect::<Vec<_>>(),
        ["b"]
    );
    let rest = ledger
        .list_events(None, first.next.as_deref())
        .await
        .unwrap();
    assert_eq!(
        rest.events.iter().map(|e| &*e.name).collect::<Vec<_>>(),
        ["a"]
    );
    assert_eq!(rest.next, None);
}

#[tokio::test]
async fn a_cursor_from_another_incarnation_is_refused() {
    let (one, two) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let other = open(one.path(), new_env_ref()).await;
    emit(&other, named("a")).await.unwrap();
    emit(&other, named("b")).await.unwrap();
    let cursor = other
        .list_events(1.try_into().ok(), None)
        .await
        .unwrap()
        .next;
    let ledger = open(two.path(), new_env_ref()).await;
    emit(&ledger, named("a")).await.unwrap();
    let Err(ReadError::Refused(r)) = ledger.list_events(None, cursor.as_deref()).await else {
        panic!("another incarnation's cursor was accepted");
    };
    assert_eq!(
        (r.kind(), r.message()),
        (ErrorKind::Invalid, "invalid cursor")
    );
}

#[tokio::test]
async fn the_ledger_errors_are_rust_errors_that_say_what_failed() {
    // D84: `Display` carries the inner text, so no error has a `source()`.
    fn shown(e: &impl std::error::Error) -> String {
        assert!(e.source().is_none(), "{e} has a source()");
        e.to_string()
    }
    let dir = tempfile::tempdir().unwrap();
    let ledger = open(dir.path(), new_env_ref()).await;
    let Err(SubmitError::Refused(r)) = emit(&ledger, named("a..b")).await else {
        panic!("a bad name was accepted");
    };
    assert_eq!(shown(&r), "event name has an empty segment");
    assert_eq!(
        shown(&SubmitError::Refused(r.clone())),
        "event name has an empty segment"
    );
    assert_eq!(
        shown(&ReadError::Refused(r)),
        "event name has an empty segment"
    );
    assert_eq!(
        shown(&SubmitError::Unknown),
        "the submission's outcome is unknown, and the ledger is unavailable until it is reopened"
    );
    assert_eq!(
        shown(&SubmitError::Unavailable),
        "the ledger is unavailable until it is reopened"
    );
    assert_eq!(
        shown(&ReadError::Unavailable),
        "the ledger is unavailable until it is reopened"
    );
}

/// Submits an execution command as the tests' embedder: execution rules' outcome.
async fn execute(
    ledger: &Ledger,
    cmd: ironflowx_execution::Command,
) -> Result<ironflowx_execution::Outcome, SubmitError> {
    match ledger
        .submit(Submission::Execution(cmd), embedder())
        .await?
        .outcome
    {
        Outcome::Execution(o) => Ok(o),
        Outcome::Events(items) => panic!("a command answered {items:?}"),
    }
}

/// RUN-2: a run's completion and its end event are one record, the event sequenced after every
/// stored event; RUN-11: a read store rebuilt from the log answers the same.
#[tokio::test]
async fn a_runs_end_and_its_event_are_one_record() {
    use ironflowx_execution::{Command, Definition, Outcome as Answer};
    let dir = tempfile::tempdir().unwrap();
    let env = new_env_ref();
    let ledger = open(dir.path(), env).await;
    emit(&ledger, named("before")).await.unwrap();
    let put = Command::PutFunction(Definition {
        name: "charge".into(),
        code: "v1".into(),
        triggers: vec![],
        max_attempts: None,
        first_delay_seconds: None,
        max_delay_seconds: None,
    });
    execute(&ledger, put).await.unwrap();
    let start = Command::StartRun {
        function: "charge".into(),
    };
    let Answer::RunStarted { run, .. } = execute(&ledger, start).await.unwrap() else {
        panic!("a run");
    };
    let claim = ironflowx_execution::Command::Claim {
        functions: vec!["charge@v1".into()],
        lease_seconds: None,
        wait_seconds: None,
    };
    let Answer::Claimed(Some(claim)) = execute(&ledger, claim).await.unwrap() else {
        panic!("a claim");
    };
    let complete = Command::Complete {
        lease: claim.lease.to_string(),
        steps: 0,
        output: Some(b"[1]".to_vec()),
    };
    let ended = execute(&ledger, complete.clone()).await.unwrap();
    assert!(matches!(ended, Answer::RunEnded(_)));
    // Sent again: answered, and nothing appended.
    let (_, watermark) = {
        ledger.close().await;
        log_records(dir.path())
    };
    let ledger = open(dir.path(), env).await;
    assert_eq!(execute(&ledger, complete).await.unwrap(), ended);
    let read = ledger.get_run(&run.to_string()).await.unwrap();
    ledger.close().await;
    let (records, after) = log_records(dir.path());
    assert_eq!(after, watermark, "a repeated completion appends nothing");
    let Some(record::Decoded::Entries { entries, .. }) = records.last() else {
        panic!("a record of entries");
    };
    // The one execution entry is the run's completion with its output: kind 6, the run's ID, `[1]`.
    let [completion] = entries.execution.as_slice() else {
        panic!("one execution entry: {entries:?}");
    };
    let mut written = Vec::new();
    ironflowx_execution::encode(completion, &mut written);
    let mut expected = vec![6, 36, 0, 0, 0];
    expected.extend_from_slice(run.to_string().as_bytes());
    expected.extend_from_slice(&[3, 0, 0, 0]);
    expected.extend_from_slice(b"[1]");
    assert_eq!(written, expected);
    let [event] = entries.record.events.as_slice() else {
        panic!("one event: {entries:?}");
    };
    assert_eq!(
        (event.name.as_str(), event.sequence),
        ("ironflowx.run.completed", 2)
    );
    std::fs::remove_file(dir.path().join("read.db")).unwrap();
    let ledger = open(dir.path(), env).await;
    assert_eq!(ledger.get_run(&run.to_string()).await.unwrap(), read);
    assert_eq!(names(&ledger).await, ["ironflowx.run.completed", "before"]);
    ledger.close().await;
}

/// A recorded step asked about again is answered its output and appends nothing; so is its
/// completion sent again.
#[tokio::test]
async fn a_replayed_step_appends_nothing() {
    use ironflowx_execution::{Begun, Command, Definition, Outcome as Answer};
    let dir = tempfile::tempdir().unwrap();
    let env = new_env_ref();
    let ledger = open(dir.path(), env).await;
    let put = Command::PutFunction(Definition {
        name: "charge".into(),
        code: "v1".into(),
        triggers: vec![],
        max_attempts: None,
        first_delay_seconds: None,
        max_delay_seconds: None,
    });
    execute(&ledger, put).await.unwrap();
    let start = Command::StartRun {
        function: "charge".into(),
    };
    execute(&ledger, start).await.unwrap();
    let claim = ironflowx_execution::Command::Claim {
        functions: vec!["charge@v1".into()],
        lease_seconds: None,
        wait_seconds: None,
    };
    let Answer::Claimed(Some(claim)) = execute(&ledger, claim).await.unwrap() else {
        panic!("a claim");
    };
    let lease = claim.lease.to_string();
    let begin = Command::BeginStep {
        lease: lease.clone(),
        position: 1,
        name: "reserve".into(),
        once: false,
        input: None,
    };
    let output = br#"{"id":  9007199254740993}"#.to_vec();
    let complete = Command::CompleteStep {
        lease,
        position: 1,
        output: Some(output.clone()),
    };
    execute(&ledger, begin.clone()).await.unwrap();
    execute(&ledger, complete.clone()).await.unwrap();
    ledger.close().await;
    let (_, before) = log_records(dir.path());
    let ledger = open(dir.path(), env).await;
    let replayed = execute(&ledger, begin).await.unwrap();
    assert_eq!(
        replayed,
        Answer::StepBegun(Begun::Completed {
            output: output.clone()
        })
    );
    assert_eq!(
        execute(&ledger, complete).await.unwrap(),
        Answer::StepCompleted { output }
    );
    ledger.close().await;
    assert_eq!(log_records(dir.path()).1, before);
}

fn put(name: &str, code: &str, triggers: &[&str]) -> ironflowx_execution::Command {
    ironflowx_execution::Command::PutFunction(ironflowx_execution::Definition {
        name: name.into(),
        code: code.into(),
        triggers: triggers.iter().map(|t| (*t).to_owned()).collect(),
        max_attempts: None,
        first_delay_seconds: None,
        max_delay_seconds: None,
    })
}

fn start(function: &str) -> ironflowx_execution::Command {
    ironflowx_execution::Command::StartRun {
        function: function.into(),
    }
}

/// A claim of `pairs`: the run it took, if any.
async fn claim_of(ledger: &Ledger, pairs: &[&str]) -> Option<ironflowx_core::RunId> {
    let cmd = ironflowx_execution::Command::Claim {
        functions: pairs.iter().map(|p| (*p).to_owned()).collect(),
        lease_seconds: None,
        wait_seconds: None,
    };
    match execute(ledger, cmd).await.unwrap() {
        ironflowx_execution::Outcome::Claimed(claim) => claim.map(|c| c.lease.run),
        other => panic!("a claim answered {other:?}"),
    }
}

async fn started(ledger: &Ledger, function: &str) -> ironflowx_core::RunId {
    match execute(ledger, start(function)).await.unwrap() {
        ironflowx_execution::Outcome::RunStarted { run, .. } => run,
        other => panic!("a start answered {other:?}"),
    }
}

/// RUN-7 on the read store: a claim takes a run only of a pair it names, name and label together,
/// and of the runs it may take, the first created when they are due alike.
#[tokio::test]
async fn a_claim_takes_a_run_of_a_pair_it_names_whole() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = open(dir.path(), new_env_ref()).await;
    execute(&ledger, put("charge", "v1", &[])).await.unwrap();
    execute(&ledger, put("refund", "v2", &[])).await.unwrap();
    // Created against name order, so neither the names nor the pairs give creation order.
    let refund = started(&ledger, "refund").await;
    let charge = started(&ledger, "charge").await;
    // Each name with the other's label: neither run is of such a pair.
    assert_eq!(claim_of(&ledger, &["charge@v2", "refund@v1"]).await, None);
    // Both due at the same time: creation order.
    let both = ["charge@v1", "refund@v2"];
    assert_eq!(claim_of(&ledger, &both).await, Some(refund));
    assert_eq!(claim_of(&ledger, &both).await, Some(charge));
    assert_eq!(claim_of(&ledger, &both).await, None);
    ledger.close().await;
}

/// A put that changes a function replaces its triggers whole, on the read store and after a
/// rebuild.
#[tokio::test]
async fn a_changed_put_replaces_the_definition_whole() {
    let dir = tempfile::tempdir().unwrap();
    let env = new_env_ref();
    let ledger = open(dir.path(), env).await;
    execute(&ledger, put("charge", "v1", &["b", "a"]))
        .await
        .unwrap();
    execute(&ledger, put("charge", "v2", &["a"])).await.unwrap();
    let f = ledger.get_function("charge").await.unwrap();
    assert_eq!(
        (f.definition.code.as_str(), f.definition.triggers.as_slice()),
        ("v2", ["a".to_owned()].as_slice())
    );
    ledger.close().await;
    std::fs::remove_file(dir.path().join("read.db")).unwrap();
    let ledger = open(dir.path(), env).await;
    assert_eq!(ledger.get_function("charge").await.unwrap(), f);
    ledger.close().await;
}

/// A step the run counts that the read store lost fails the ledger, as any store failure does;
/// a rebuilt read store has it again.
#[tokio::test]
async fn a_lost_step_fails_the_ledger() {
    use ironflowx_execution::Command;
    let dir = tempfile::tempdir().unwrap();
    let env = new_env_ref();
    let ledger = open(dir.path(), env).await;
    execute(&ledger, put("charge", "v1", &[])).await.unwrap();
    let run = started(&ledger, "charge").await;
    claim_of(&ledger, &["charge@v1"]).await.unwrap();
    let begin = Command::BeginStep {
        lease: format!("{run}.1"),
        position: 1,
        name: "reserve".into(),
        once: false,
        input: None,
    };
    execute(&ledger, begin.clone()).await.unwrap();
    // A read waits for the fold of every acknowledged record, so the step is there to delete.
    assert_eq!(ledger.get_run(&run.to_string()).await.unwrap().steps, 1);
    let deleted = ledger
        .store
        .lock()
        .unwrap()
        .execute("DELETE FROM steps", [])
        .unwrap();
    assert_eq!(deleted, 1);
    assert_eq!(
        execute(&ledger, begin.clone()).await,
        Err(SubmitError::Unavailable)
    );
    assert_eq!(ledger.state(), LedgerState::Unavailable);
    ledger.close().await;
    // Its rows deleted, its tables whole: open keeps it, and only a rebuild finds the step again.
    std::fs::remove_file(dir.path().join("read.db")).unwrap();
    let ledger = open(dir.path(), env).await;
    let again = execute(&ledger, begin).await.unwrap();
    let run_again = ironflowx_execution::Begun::Run { attempt: 1 };
    assert_eq!(again, ironflowx_execution::Outcome::StepBegun(run_again));
    ledger.close().await;
}

/// The fold writes only what follows from the store: an entry about a run it lacks, or a second
/// output for a step, fails its transaction and changes nothing (RUN-3's fold half).
#[test]
fn the_fold_refuses_an_entry_the_store_does_not_follow() {
    use ironflowx_execution::Entry;
    let dir = tempfile::tempdir().unwrap();
    let env = new_env_ref();
    let ledger = Ledger::open_blocking(dir.path(), env, clock()).unwrap();
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let run = rt.block_on(async {
        execute(&ledger, put("charge", "v1", &[])).await.unwrap();
        let run = started(&ledger, "charge").await;
        claim_of(&ledger, &["charge@v1"]).await.unwrap();
        let begin = ironflowx_execution::Command::BeginStep {
            lease: format!("{run}.1"),
            position: 1,
            name: "reserve".into(),
            once: false,
            input: None,
        };
        execute(&ledger, begin).await.unwrap();
        let complete = ironflowx_execution::Command::CompleteStep {
            lease: format!("{run}.1"),
            position: 1,
            output: Some(b"1".to_vec()),
        };
        execute(&ledger, complete).await.unwrap();
        // A read waits for the fold of every acknowledged record, so the output is there.
        ledger.get_run(&run.to_string()).await.unwrap();
        run
    });
    let unknown = "0190b5f0-0000-7000-8000-000000000999".parse().unwrap();
    let mut conn = ledger.store.lock().unwrap();
    let applied = crate::store::applied(&conn).unwrap();
    for entry in [
        Entry::Claimed {
            run: unknown,
            execution: 1,
            lease_until: Timestamp::from_millis(T),
        },
        Entry::StepCompleted {
            run,
            position: 1,
            output: b"2".to_vec(),
        },
    ] {
        let entries = Entries {
            record: ironflowx_events::Record {
                time: Timestamp::from_millis(T),
                events: vec![],
            },
            execution: vec![entry.clone().into()],
        };
        let record = Some((uuid::Uuid::now_v7(), &entries, &emitter()));
        let folded = crate::store::apply(&mut conn, applied, record, false);
        assert!(folded.is_err(), "{entry:?}");
        assert_eq!(crate::store::applied(&conn).unwrap(), applied, "{entry:?}");
    }
    let output: Vec<u8> = conn
        .query_row("SELECT output FROM steps", [], |r| r.get(0))
        .unwrap();
    assert_eq!(output, b"1");
    drop(conn);
    drop(ledger);
}

/// RUN-10 at the read store: a queued run is claimable once its due time is reached, not a
/// millisecond before.
#[test]
fn a_run_is_claimable_from_its_due_time() {
    use ironflowx_execution::{Entry, ExecutionView};
    let dir = tempfile::tempdir().unwrap();
    let ledger = Ledger::open_blocking(dir.path(), new_env_ref(), clock()).unwrap();
    let run: ironflowx_core::RunId = "0190b5f0-0000-7000-8000-000000000123".parse().unwrap();
    let mut conn = ledger.store.lock().unwrap();
    let applied = crate::store::applied(&conn).unwrap();
    let created = Entry::RunCreated {
        run,
        sequence: 1,
        function: "charge".into(),
        code: "v1".into(),
        input: b"null".to_vec(),
    };
    // A run created at T + 1,000 is due then.
    let entries = Entries {
        record: ironflowx_events::Record {
            time: Timestamp::from_millis(T + 1_000),
            events: vec![],
        },
        execution: vec![created.into()],
    };
    let record = Some((uuid::Uuid::now_v7(), &entries, &emitter()));
    crate::store::apply(&mut conn, applied, record, false).unwrap();
    let pairs = [("charge".to_owned(), "v1".to_owned())];
    let at = |millis| {
        let taken = ReadStore(&conn).claimable(&pairs, Timestamp::from_millis(millis));
        taken.unwrap().map(|r| r.id)
    };
    assert_eq!(at(T + 999), None);
    assert_eq!(at(T + 1_000), Some(run));
    drop(conn);
    drop(ledger);
}

/// A waiting claim's read at the read store: the earliest due time of a queued run of its pairs,
/// due or not, and none of another pair's.
#[test]
fn the_earliest_due_time_is_of_a_pair_the_claim_names() {
    use ironflowx_execution::Entry;
    let dir = tempfile::tempdir().unwrap();
    let ledger = Ledger::open_blocking(dir.path(), new_env_ref(), clock()).unwrap();
    let mut conn = ledger.store.lock().unwrap();
    // refund@v1 due at T + 500, then charge@v1 at T + 2,000 and at T + 1,000.
    for (n, function, due) in [
        (1, "refund", 500),
        (2, "charge", 2_000),
        (3, "charge", 1_000),
    ] {
        let applied = crate::store::applied(&conn).unwrap();
        let created = Entry::RunCreated {
            run: format!("0190b5f0-0000-7000-8000-00000000012{n}")
                .parse()
                .unwrap(),
            sequence: n,
            function: function.into(),
            code: "v1".into(),
            input: b"null".to_vec(),
        };
        let entries = Entries {
            record: ironflowx_events::Record {
                time: Timestamp::from_millis(T + due),
                events: vec![],
            },
            execution: vec![created.into()],
        };
        let record = Some((uuid::Uuid::now_v7(), &entries, &emitter()));
        crate::store::apply(&mut conn, applied, record, false).unwrap();
    }
    let due = |pairs: &[(&str, &str)]| {
        let pairs: Vec<_> = pairs
            .iter()
            .map(|(n, c)| ((*n).to_owned(), (*c).to_owned()))
            .collect();
        ReadStore(&conn).earliest_due(&pairs).unwrap()
    };
    let at = |ms| Some(Timestamp::from_millis(T + ms));
    assert_eq!(due(&[("charge", "v1")]), at(1_000));
    assert_eq!(due(&[("charge", "v2"), ("refund", "v1")]), at(500));
    assert_eq!(due(&[("charge", "v2")]), None);
    // Two pairs, each with a queued run: the earlier of the two.
    assert_eq!(due(&[("charge", "v1"), ("refund", "v1")]), at(500));
    drop(conn);
    drop(ledger);
}

/// A waiting claim's wake: `Changes` resolves once a commit is folded, once for it, and at a
/// failure; after a close it never does, as the close is no change.
#[tokio::test]
async fn changes_resolve_at_a_commit_and_a_failure_and_never_after_a_close() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = open(dir.path(), new_env_ref()).await;
    let mut changes = ledger.changes();
    assert!(!ready(std::pin::pin!(changes.changed())));
    let a = emit(&ledger, named("a")).await.unwrap();
    // Folded, by a read, so the hold below holds b's fold, not a's.
    get(&ledger, a).await.unwrap();
    assert!(ready(std::pin::pin!(changes.changed())), "a fold");
    assert!(
        !ready(std::pin::pin!(changes.changed())),
        "a fold tells once"
    );
    // Taken once b is committed, and before its fold fails: a commit is no change until folded.
    ledger.hold_fold();
    ledger.fail_next_fold();
    emit(&ledger, named("b")).await.unwrap();
    let mut at_fold = ledger.changes();
    assert!(!ready(std::pin::pin!(at_fold.changed())));
    assert!(
        !ready(std::pin::pin!(changes.changed())),
        "b committed, not folded"
    );
    ledger.release_fold();
    ledger.faulted().await;
    assert!(ready(std::pin::pin!(at_fold.changed())), "a failure");
    // A failed ledger's claim read is refused, as every read is.
    let claim = ironflowx_execution::Command::Claim {
        functions: vec!["charge@v1".into()],
        lease_seconds: None,
        wait_seconds: None,
    };
    let read = ledger.claimable(&claim, &clock()).await;
    assert_eq!(read, Err(ReadError::Unavailable));
    ledger.close().await;

    let dir = tempfile::tempdir().unwrap();
    let ledger = open(dir.path(), new_env_ref()).await;
    let mut changes = ledger.changes();
    ledger.close().await;
    assert!(!ready(std::pin::pin!(changes.changed())), "a close");
}
