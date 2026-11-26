//! Creating keys through the engine: what a created key holds and says about itself, how long
//! it works, and what a creator may not grant (D27, D28, TEN-5).

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use ironflowx_cell::ManualClock;
use ironflowx_core::{Clock, ErrorKind, KeyOrigin, Scope, Secret, Timestamp};
use ironflowx_operations::Engine;
use ironflowx_proto::{
    Call, CreateKeyRequest, KeyStatus, ListEventsRequest, Operations, WhoamiRequest,
};

const T: i64 = 1_800_000_000_000;
const HOUR: i64 = 3_600_000;

const fn at(ms: i64) -> Timestamp {
    Timestamp::from_millis(ms)
}

fn scopes(s: &[Scope]) -> BTreeSet<Scope> {
    s.iter().copied().collect()
}

fn as_call(created: &ironflowx_proto::CreatedKey) -> Call {
    Call::with_key(Secret::new(created.key.expose().to_owned()))
}

const fn page() -> ListEventsRequest {
    ListEventsRequest {
        limit: None,
        cursor: None,
    }
}

#[tokio::test]
async fn an_admin_key_creates_a_key_that_works_until_its_expiry() {
    let dir = tempfile::tempdir().unwrap();
    let clock = ManualClock::new(at(T));
    let opened = Engine::open(dir.path(), clock.clone(), None).await.unwrap();
    let engine = &opened.engine;
    let admin = Call::with_key(opened.minted.unwrap());
    let admin_id = engine
        .whoami(&admin, WhoamiRequest {})
        .await
        .unwrap()
        .key_id;

    clock.set(at(T + 5));
    let created = engine
        .create_key(
            &admin,
            CreateKeyRequest {
                scopes: BTreeSet::new(),
                expires_at: Some(at(T + HOUR)),
                label: Some("agent-7".into()),
            },
        )
        .await
        .unwrap();
    let info = &created.info;
    assert_eq!(
        info.scopes,
        scopes(&[Scope::Read]),
        "no scopes asked: {{read}}"
    );
    assert_eq!(info.label.as_deref(), Some("agent-7"));
    assert_eq!(
        (info.origin, info.created_by, info.status),
        (KeyOrigin::Created, Some(admin_id), KeyStatus::Active)
    );
    assert_eq!(
        (info.created_at, info.expires_at),
        (at(T + 5), Some(at(T + HOUR)))
    );
    assert_eq!(created.key.expose()[4..40], info.key_id.to_string());

    let key = as_call(&created);
    let me = engine.whoami(&key, WhoamiRequest {}).await.unwrap();
    assert_eq!(
        (me.key_id, me.expires_at),
        (info.key_id, Some(at(T + HOUR)))
    );
    clock.set(at(T + HOUR - 1));
    engine
        .list_events(&key, page())
        .await
        .expect("before its expiry");
    clock.set(at(T + HOUR));
    let err = engine
        .list_events(&key, page())
        .await
        .expect_err("from its expiry");
    assert_eq!(
        (err.kind, err.message.as_str(), err.hint),
        (ErrorKind::Unauthenticated, "invalid API key", None)
    );
    opened.engine.close().await;
}

#[tokio::test]
async fn a_key_grants_nothing_beyond_its_creator() {
    let dir = tempfile::tempdir().unwrap();
    let clock = ManualClock::new(at(T));
    let opened = Engine::open(dir.path(), clock.clone(), None).await.unwrap();
    let engine = &opened.engine;
    let admin = Call::with_key(opened.minted.unwrap());
    let creator = engine
        .create_key(
            &admin,
            CreateKeyRequest {
                scopes: scopes(&[Scope::Admin, Scope::Read]),
                expires_at: Some(at(T + HOUR)),
                label: None,
            },
        )
        .await
        .unwrap();
    let creator = as_call(&creator);

    for (case, scopes, expires_at, label) in [
        (
            "a scope it lacks",
            scopes(&[Scope::Write]),
            Some(T + 1),
            None,
        ),
        ("no expiry", scopes(&[Scope::Read]), None, None),
        ("after it", scopes(&[Scope::Read]), Some(T + HOUR + 1), None),
        ("now", scopes(&[Scope::Read]), Some(T), None),
        (
            "a label of 129 bytes",
            scopes(&[Scope::Read]),
            Some(T + 1),
            Some("x".repeat(129)),
        ),
    ] {
        let req = CreateKeyRequest {
            scopes,
            expires_at: expires_at.map(at),
            label,
        };
        let err = engine.create_key(&creator, req).await.expect_err(case);
        assert_eq!(err.kind, ErrorKind::Invalid, "{case}: {err:?}");
    }
    let req = CreateKeyRequest {
        scopes: scopes(&[Scope::Read]),
        expires_at: Some(at(T + HOUR)),
        label: None,
    };
    engine
        .create_key(&creator, req)
        .await
        .expect("its own expiry");
    opened.engine.close().await;
}

/// A clock that, once given a step, moves on by it after each reading.
#[derive(Clone)]
struct Stepping {
    now: Arc<AtomicI64>,
    step: Arc<AtomicI64>,
}

impl Clock for Stepping {
    fn now(&self) -> Timestamp {
        at(self
            .now
            .fetch_add(self.step.load(Ordering::SeqCst), Ordering::SeqCst))
    }
}

/// The creator is valid when the call is admitted, then expired when its key would be stored:
/// the insert's own check refuses it, with the answer an expired key gets.
#[tokio::test]
async fn a_creator_that_expires_before_its_key_is_stored_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let clock = Stepping {
        now: Arc::new(AtomicI64::new(T)),
        step: Arc::new(AtomicI64::new(0)),
    };
    let opened = Engine::open(dir.path(), clock.clone(), None).await.unwrap();
    let engine = &opened.engine;
    let admin = Call::with_key(opened.minted.unwrap());
    let creator = CreateKeyRequest {
        scopes: scopes(&[Scope::Admin, Scope::Read]),
        expires_at: Some(at(T + HOUR)),
        label: None,
    };
    let creator = as_call(&engine.create_key(&admin, creator).await.unwrap());

    // Admission reads T; the insert, an hour on, when the creator has expired. First with the
    // recording of its expiry failing: the answer is the same, and the clock set back accepts it.
    let db = rusqlite::Connection::open(dir.path().join("control.db")).unwrap();
    db.execute_batch(
        "CREATE TRIGGER fail BEFORE UPDATE ON keys BEGIN SELECT RAISE(ABORT, 'injected'); END;",
    )
    .unwrap();
    for recording in ["fails", "fails at commit", "works"] {
        clock.now.store(T, Ordering::SeqCst);
        clock.step.store(HOUR, Ordering::SeqCst);
        let req = CreateKeyRequest {
            scopes: scopes(&[Scope::Read]),
            expires_at: Some(at(T + HOUR)),
            label: None,
        };
        let err = engine.create_key(&creator, req).await.expect_err("refused");
        assert_eq!(
            (err.kind, err.message.as_str(), err.hint),
            (ErrorKind::Unauthenticated, "invalid API key", None),
            "recording {recording}"
        );
        clock.now.store(T, Ordering::SeqCst);
        clock.step.store(0, Ordering::SeqCst);
        let back = engine.whoami(&creator, WhoamiRequest {}).await;
        if recording == "works" {
            let err = back.expect_err("its expiry was recorded");
            assert_eq!(
                (err.kind, err.message.as_str(), err.hint),
                (ErrorKind::Unauthenticated, "invalid API key", None)
            );
        } else {
            back.expect(recording);
        }
        db.execute_batch("DROP TRIGGER IF EXISTS fail").unwrap();
        if recording == "fails" {
            // The update works, and breaks a constraint SQLite checks at the commit.
            db.execute_batch(
                "CREATE TABLE late (t REFERENCES tenants(id) DEFERRABLE INITIALLY DEFERRED);
                 CREATE TRIGGER fail AFTER UPDATE ON keys
                 BEGIN INSERT INTO late VALUES ('none'); END;",
            )
            .unwrap();
        }
    }
    opened.engine.close().await;
}

/// A trigger that aborts the insert leaves only the bootstrap key and returns `Unavailable`.
/// After the trigger is removed, the same open engine accepts a new key.
#[tokio::test]
async fn a_failed_insert_is_unavailable_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let opened = Engine::open(dir.path(), ManualClock::new(at(T)), None)
        .await
        .unwrap();
    let engine = &opened.engine;
    let admin = Call::with_key(opened.minted.unwrap());
    let db = rusqlite::Connection::open(dir.path().join("control.db")).unwrap();
    let count = || -> i64 {
        db.query_row("SELECT count(*) FROM keys", [], |r| r.get(0))
            .unwrap()
    };
    db.execute_batch(
        "CREATE TRIGGER fail BEFORE INSERT ON keys BEGIN SELECT RAISE(ABORT, 'injected'); END;",
    )
    .unwrap();

    let err = engine
        .create_key(&admin, CreateKeyRequest::default())
        .await
        .expect_err("the insert fails");
    assert_eq!(err.kind, ErrorKind::Unavailable, "{err:?}");
    assert!(err.kind.retryable());
    assert_eq!(count(), 1, "only the bootstrap key");

    db.execute_batch("DROP TRIGGER fail;").unwrap();
    engine
        .create_key(&admin, CreateKeyRequest::default())
        .await
        .expect("the retry");
    assert_eq!(count(), 2);
    opened.engine.close().await;
}
