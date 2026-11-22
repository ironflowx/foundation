//! The deterministic tests of invariants.md, through `Ledger` and `LocalOperations`.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

use ironflowx_core::{Emitter, EnvRef, ErrorKind, SubmitAs, Timestamp};
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

/// What `LocalOperations` attributes every submission to.
fn local() -> Emitter {
    "embedder:local-operations".parse().unwrap()
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
            crate::store::apply(&mut conn, offset, Some((&record, &local())), false).unwrap();
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
        let foreign = record::events(new_env_ref().token(), &local(), &record);
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
            record::events(env.token(), &local(), &record),
        );
    }
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
        emit(&ledger, keyed("a", "k")).await.unwrap();
        ledger.close().await;
        let conn = rusqlite::Connection::open(dir.path().join("read.db")).unwrap();
        conn.execute_batch(damage).unwrap();
        drop(conn);
        let ledger = open(dir.path(), env).await;
        assert_eq!(names(&ledger).await, ["a"], "{damage}");
        let retry = LocalOperations::new(&ledger)
            .emit(&Call::anonymous(), keyed("b", "k"))
            .await
            .unwrap();
        assert!(retry.duplicate, "{damage}");
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
            |_| -> Result<(), ironflowx_events::Error<crate::StoreError>> { panic!("a query bug") },
        )
        .await;
    assert_eq!(answer, Err(crate::ReadError::Unavailable));
    assert_eq!(all(&ledger).await.unwrap_err().kind, ErrorKind::Unavailable);
    assert_eq!(
        emit(&ledger, named("b")).await.unwrap_err().kind,
        ErrorKind::Unavailable
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
    let page = LocalOperations::new(&ledger)
        .list_events(
            &Call::anonymous(),
            ListEventsRequest {
                limit: None,
                cursor: Some(format!("{}.{}", env.token(), u64::MAX)),
            },
        )
        .await
        .unwrap();
    assert_eq!(page.events.len(), 1);
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
    let key: Emitter = "key:019a9f3e-7c2b-7d41-8a3e-5f6b7c8d9e0f".parse().unwrap();
    for emitted_by in [local(), key] {
        assert_eq!(
            record::decode(&record::events(env.token(), &emitted_by, &record)),
            Some(record::Decoded::Events {
                format: record::FORMAT,
                token: env.token(),
                emitted_by,
                record: record.clone(),
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

/// A ledger at `dir` holding events "a" and "b", closed.
async fn two_events(dir: &Path, env: EnvRef) {
    let ledger = open(dir, env).await;
    emit(&ledger, keyed("a", "ka")).await.unwrap();
    emit(&ledger, keyed("b", "kb")).await.unwrap();
    ledger.close().await;
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
        two_events(dir.path(), env).await;
        damage(&dir.path().join("read.db"));
        let ledger = open(dir.path(), env).await;
        assert_eq!(names(&ledger).await, ["b", "a"], "{why}");
        let retry = LocalOperations::new(&ledger)
            .emit(&Call::anonymous(), keyed("x", "ka"))
            .await
            .unwrap();
        assert!(retry.duplicate, "{why}");
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
    assert_eq!(err.kind, ErrorKind::Unavailable);
    assert_eq!(all(&ledger).await.unwrap_err().kind, ErrorKind::Unavailable);
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
    let ledger = std::sync::Arc::new(open(dir.path(), env).await);
    ledger.hold_fold();
    emit(&ledger, named("a")).await.unwrap();
    let queued = tokio::spawn({
        let ledger = ledger.clone();
        async move { emit(&ledger, named("queued")).await }
    });
    // Let it reach the writer's queue.
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!queued.is_finished());
    ledger.release_fold();
    let id = queued.await.unwrap().unwrap();
    std::sync::Arc::into_inner(ledger).unwrap().close().await;
    let ledger = open(dir.path(), env).await;
    assert_eq!(get(&ledger, &id).await.unwrap().name, "queued");
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
    assert_eq!(second.await.unwrap_err().kind, ErrorKind::Unavailable);
    assert_eq!(first.await.unwrap_err().kind, ErrorKind::Unavailable);
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
    let batch = ironflowx_events::Submission::Batch(
        ["a", "b", "c"]
            .map(|name| ironflowx_events::Emit {
                name: name.into(),
                ..Default::default()
            })
            .into(),
    );
    let app = SubmitAs::embedder("billing app").unwrap();
    let committed = ledger.submit(batch, app.clone()).await.unwrap();
    let solo = emit(&ledger, named("d")).await.unwrap();
    let expected = [
        local(),
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
    let ironflowx_events::ItemOutcome::New(first) = committed.outcome[0] else {
        panic!("a new event");
    };
    let read = get(&ledger, &first.to_string()).await.unwrap();
    assert_eq!(read.emitted_by.to_string(), "embedder:billing app");
    assert_eq!(get(&ledger, &solo).await.unwrap().emitted_by, local());
    ledger.close().await;
    let ledger = open(dir.path(), env).await;
    assert_eq!(emitters(&ledger).await, expected);
    ledger.close().await;
    std::fs::remove_file(dir.path().join("read.db")).unwrap();
    let ledger = open(dir.path(), env).await;
    assert_eq!(emitters(&ledger).await, expected);
}

#[tokio::test]
async fn a_format_1_directory_is_refused_as_another_format() {
    let dir = tempfile::tempdir().unwrap();
    let env = new_env_ref();
    {
        let log = SegmentLog::open(dir.path().join("log"), Options::default()).unwrap();
        let mut genesis = record::genesis(env);
        genesis[1..3].copy_from_slice(&1u16.to_le_bytes());
        append(&log, ExpectedRevision::Empty, genesis);
    }
    let err = Ledger::open(dir.path(), env, clock()).await.err();
    assert!(matches!(err, Some(OpenError::Format)), "{err:?}");
}

#[tokio::test]
async fn local_operations_create_no_key() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = open(dir.path(), new_env_ref()).await;
    let req = ironflowx_proto::CreateKeyRequest::default();
    let err = LocalOperations::new(&ledger)
        .create_key(&Call::anonymous(), req)
        .await
        .expect_err("refused");
    assert_eq!(err.kind, ErrorKind::Unauthenticated);
    ledger.close().await;
}
