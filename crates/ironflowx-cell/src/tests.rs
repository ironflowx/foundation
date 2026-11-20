//! The deterministic tests of invariants.md, through `Ledger` and `LocalOperations`.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

use ironflowx_core::{EnvRef, ErrorKind, Timestamp};
use ironflowx_proto::{
    Call, EmitRequest, Event, GetEventRequest, ListEventsRequest, OpError, Operations,
};
use segment_log_core::{ExpectedRevision, NewRecord, Options, SegmentLog, Submit};

use crate::{Ledger, LocalOperations, ManualClock, OpenError, new_env_ref, record};

const T: i64 = 1_800_000_000_000;
const HOUR: i64 = 3_600_000;

fn clock() -> ManualClock {
    ManualClock::new(Timestamp::from_millis(T))
}

async fn open(dir: &Path, env: EnvRef) -> Ledger {
    Ledger::open(dir, env, clock()).await.expect("opens")
}

fn named(name: &str) -> EmitRequest {
    EmitRequest {
        name: name.into(),
        data: None,
        metadata: None,
        version: None,
        idempotency_key: None,
    }
}

fn keyed(name: &str, key: &str) -> EmitRequest {
    EmitRequest {
        idempotency_key: Some(key.into()),
        ..named(name)
    }
}

async fn emit(ledger: &Ledger, req: EmitRequest) -> Result<String, OpError> {
    LocalOperations::new(ledger)
        .emit(&Call::anonymous(), req)
        .await
        .map(|r| r.event_id.to_string())
}

async fn get(ledger: &Ledger, id: &str) -> Result<Event, OpError> {
    LocalOperations::new(ledger)
        .get_event(
            &Call::anonymous(),
            GetEventRequest {
                event_id: id.into(),
            },
        )
        .await
}

async fn all(ledger: &Ledger) -> Result<Vec<Event>, OpError> {
    LocalOperations::new(ledger)
        .list_events(
            &Call::anonymous(),
            ListEventsRequest {
                limit: 200.try_into().ok(),
                cursor: None,
            },
        )
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
        let other = LocalOperations::new(&ledger);
        let read = async {
            let event = other
                .get_event(
                    &Call::anonymous(),
                    GetEventRequest {
                        event_id: id.clone(),
                    },
                )
                .await;
            let page = all(&ledger).await;
            (event, page)
        };
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
        assert_eq!(page.unwrap()[0].event_id.to_string(), id);
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
        get(ledger, &id).await.unwrap().time.as_millis()
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
    let batch = ironflowx_proto::EmitBatchRequest {
        items: vec![named("a"), named("b")],
    };
    LocalOperations::new(&ledger)
        .emit_batch(&Call::anonymous(), batch)
        .await
        .unwrap();
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
        let record = ironflowx_events::Record {
            time: Timestamp::from_millis(T),
            events: vec![],
        };
        for offset in 0..applied {
            crate::store::apply(&mut conn, offset, Some(&record)).unwrap();
        }
        assert_eq!(crate::store::applied(&conn).unwrap(), applied);
    }
    assert_eq!(all(&ledger).await.unwrap(), before);
    ledger.close().await;
    std::fs::remove_file(dir.path().join("read.db")).unwrap();
    let ledger = open(dir.path(), env).await;
    assert_eq!(all(&ledger).await.unwrap(), before);
    assert_eq!(
        emit(&ledger, keyed("x", "a")).await.unwrap(),
        before[2].event_id.to_string()
    );
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
        let foreign = record::events(new_env_ref().token(), &record);
        append(&log, ExpectedRevision::At(revision), foreign);
    }
    std::fs::remove_file(dir.path().join("read.db")).unwrap();
    let err = Ledger::open(dir.path(), env, clock()).await.err();
    assert!(matches!(err, Some(OpenError::Corrupt)), "{err:?}");
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
        let read = get(&ledger, &id);
        tokio::pin!(read);
        assert!(
            tokio::time::timeout(Duration::from_millis(100), &mut read)
                .await
                .is_err()
        );
        ledger.release_fold();
        assert_eq!(read.await.unwrap_err().kind, ErrorKind::Unavailable);
    }
    assert_eq!(
        emit(&ledger, named("b")).await.unwrap_err().kind,
        ErrorKind::Unavailable
    );
    ledger.close().await;
    let ledger = open(dir.path(), env).await;
    assert_eq!(get(&ledger, &id).await.unwrap().name, "a");
}

#[tokio::test]
async fn an_unknown_append_is_recovered_by_a_keyed_retry() {
    for landed in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let env = new_env_ref();
        let ledger = open(dir.path(), env).await;
        ledger.fail_next_append(landed);
        let err = emit(&ledger, keyed("a", "k")).await.unwrap_err();
        assert_eq!(err.kind, ErrorKind::Unknown);
        let err = emit(&ledger, keyed("a", "k")).await.unwrap_err();
        assert_eq!(err.kind, ErrorKind::Unavailable);
        assert_eq!(all(&ledger).await.unwrap_err().kind, ErrorKind::Unavailable);
        ledger.close().await;

        let ledger = open(dir.path(), env).await;
        let before = all(&ledger).await.unwrap();
        assert_eq!(before.len(), usize::from(landed));
        let retry = LocalOperations::new(&ledger)
            .emit(&Call::anonymous(), keyed("a", "k"))
            .await
            .unwrap();
        assert_eq!(retry.duplicate, landed);
        let after = all(&ledger).await.unwrap();
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].event_id, retry.event_id);
    }
}

#[tokio::test]
async fn a_refused_emit_leaves_nothing_behind() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = open(dir.path(), new_env_ref()).await;
    let err = emit(&ledger, named("a..b")).await.unwrap_err();
    assert_eq!(err.kind, ErrorKind::Invalid);
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
    assert_eq!(
        record::decode(&record::events(env.token(), &record)),
        Some(record::LogRecord::Events {
            format: record::FORMAT,
            token: env.token(),
            record
        })
    );
    assert_eq!(
        record::decode(&record::genesis(env)),
        Some(record::LogRecord::Genesis {
            format: record::FORMAT,
            env
        })
    );
    let mut trailing = record::genesis(env);
    trailing.push(0);
    assert_eq!(record::decode(&trailing), None);
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
