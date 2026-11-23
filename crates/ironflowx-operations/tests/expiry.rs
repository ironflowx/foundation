//! A key refused as expired stays expired (D42, TEN-13), and created keys hold slots until they
//! are revoked or their expiry is recorded (TEN-15), through the engine on a `ManualClock`.

use std::path::Path;

use ironflowx_cell::ManualClock;
use ironflowx_core::{Clock, ErrorKind, Scope, Secret, Timestamp};
use ironflowx_operations::Engine;
use ironflowx_proto::{
    Call, CreateKeyRequest, KeyStatus, ListKeysRequest, OpError, RevokeKeyRequest, WhoamiRequest,
};

const T: i64 = 1_800_000_000_000;
const HOUR: i64 = 3_600_000;
const YEAR: i64 = 365 * 24 * HOUR;

const fn at(ms: i64) -> Timestamp {
    Timestamp::from_millis(ms)
}

fn call(key: &str) -> Call {
    Call::with_key(Secret::new(key.to_owned()))
}

/// Opens `dir` for the first time; the engine and the bootstrap key's text.
async fn open(dir: &Path, clock: impl Clock + Clone + Send + Sync + 'static) -> (Engine, String) {
    let opened = Engine::open(dir, clock).await.unwrap();
    (opened.engine, opened.minted.unwrap().expose().to_owned())
}

/// `by` creates a key holding `scopes` until `expires_at`; its text.
async fn create(engine: &Engine, by: &str, scopes: &[Scope], expires_at: i64) -> String {
    let req = CreateKeyRequest {
        scopes: scopes.iter().copied().collect(),
        expires_at: Some(at(expires_at)),
        label: None,
    };
    let created = engine.create_key(&call(by), req).await.unwrap();
    created.key.expose().to_owned()
}

async fn assert_works(engine: &Engine, key: &str, case: &str) {
    engine
        .whoami(&call(key), WhoamiRequest {})
        .await
        .expect(case);
}

async fn assert_refused(engine: &Engine, key: &str, case: &str) {
    let answer = engine.whoami(&call(key), WhoamiRequest {}).await;
    assert_uniform(&answer.expect_err(case), case);
}

/// The one answer for a key that is not valid.
fn assert_uniform(err: &OpError, case: &str) {
    assert_eq!(
        (err.kind, err.message.as_str(), &err.hint),
        (ErrorKind::Unauthenticated, "invalid API key", &None),
        "{case}"
    );
}

async fn status(engine: &Engine, by: &str, key: &str) -> KeyStatus {
    let page = engine
        .list_keys(&call(by), ListKeysRequest::default())
        .await
        .unwrap();
    let listed = page
        .keys
        .iter()
        .find(|k| k.key_id.to_string() == key[4..40]);
    listed.expect("listed").status
}

fn db(dir: &Path) -> rusqlite::Connection {
    rusqlite::Connection::open(dir.join("control.db")).unwrap()
}

/// Makes every update of a key row fail until [`mend`].
fn fail_recording(dir: &Path) {
    db(dir)
        .execute_batch(
            "CREATE TRIGGER fail BEFORE UPDATE ON keys BEGIN SELECT RAISE(ABORT, 'injected'); END;",
        )
        .unwrap();
}

fn mend(dir: &Path) {
    db(dir).execute_batch("DROP TRIGGER fail").unwrap();
}

#[tokio::test]
async fn a_key_refused_as_expired_stays_refused_when_the_clock_steps_back_and_after_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let clock = ManualClock::new(at(T));
    let (engine, admin) = open(dir.path(), clock.clone()).await;
    let key = create(&engine, &admin, &[Scope::Read], T + HOUR).await;
    assert_works(&engine, &key, "before its expiry").await;
    clock.set(at(T + HOUR));
    assert_refused(&engine, &key, "at its expiry").await;
    clock.set(at(T));
    assert_refused(&engine, &key, "an hour back").await;
    assert_eq!(status(&engine, &admin, &key).await, KeyStatus::Expired);
    engine.close().await;

    let engine = Engine::open(dir.path(), clock).await.unwrap().engine;
    assert_refused(&engine, &key, "after a restart").await;
    engine.close().await;
}

/// The clock runs a year ahead, past two keys' expiry, and is corrected: only the key refused
/// meanwhile stays expired. Listing records nothing, for the key that lists or those listed.
#[tokio::test]
async fn a_clock_ahead_expires_only_the_keys_refused_meanwhile() {
    let dir = tempfile::tempdir().unwrap();
    let clock = ManualClock::new(at(T));
    let (engine, admin) = open(dir.path(), clock.clone()).await;
    let used = create(&engine, &admin, &[Scope::Read], T + HOUR).await;
    let idle = create(&engine, &admin, &[Scope::Read], T + HOUR).await;
    clock.set(at(T + YEAR));
    assert_refused(&engine, &used, "ahead").await;
    for key in [&used, &idle] {
        assert_eq!(status(&engine, &admin, key).await, KeyStatus::Expired);
    }
    clock.set(at(T));
    assert_refused(&engine, &used, "corrected").await;
    assert_works(&engine, &idle, "listed, not refused").await;
    assert_eq!(status(&engine, &admin, &used).await, KeyStatus::Expired);
    assert_eq!(status(&engine, &admin, &idle).await, KeyStatus::Active);
    engine.close().await;
}

#[tokio::test]
async fn a_creators_recorded_expiry_does_not_spread_to_its_children() {
    let dir = tempfile::tempdir().unwrap();
    let clock = ManualClock::new(at(T));
    let (engine, admin) = open(dir.path(), clock.clone()).await;
    let creator = create(&engine, &admin, &[Scope::Admin, Scope::Read], T + HOUR).await;
    let child = create(&engine, &creator, &[Scope::Read], T + HOUR).await;
    clock.set(at(T + YEAR));
    assert_refused(&engine, &creator, "ahead").await;
    clock.set(at(T + HOUR - 1));
    assert_refused(&engine, &creator, "corrected").await;
    assert_works(&engine, &child, "until its own expiry").await;
    clock.set(at(T + HOUR));
    assert_refused(&engine, &child, "at its own expiry").await;
    engine.close().await;
}

/// The stated exception: with recording failing, the refusal is the same but nothing is
/// recorded, so a clock set back accepts the key, until a later refusal records it.
#[tokio::test]
async fn an_expiry_that_fails_to_record_is_recorded_by_a_later_refusal() {
    let dir = tempfile::tempdir().unwrap();
    let clock = ManualClock::new(at(T));
    let (engine, admin) = open(dir.path(), clock.clone()).await;
    let key = create(&engine, &admin, &[Scope::Read], T + HOUR).await;
    fail_recording(dir.path());
    clock.set(at(T + HOUR));
    assert_refused(&engine, &key, "recording fails").await;
    clock.set(at(T));
    assert_works(&engine, &key, "nothing was recorded").await;

    mend(dir.path());
    clock.set(at(T + HOUR));
    assert_refused(&engine, &key, "recorded").await;
    clock.set(at(T));
    assert_refused(&engine, &key, "stays expired").await;
    engine.close().await;
}

/// A request with a wrong secret records nothing: only the key's holder can.
#[tokio::test]
async fn a_wrong_secret_records_no_expiry() {
    let dir = tempfile::tempdir().unwrap();
    let clock = ManualClock::new(at(T));
    let (engine, admin) = open(dir.path(), clock.clone()).await;
    let key = create(&engine, &admin, &[Scope::Read], T + HOUR).await;
    let wrong = format!("{}{}", &key[..41], "A".repeat(43));
    clock.set(at(T + HOUR));
    assert_refused(&engine, &wrong, "a wrong secret").await;
    clock.set(at(T));
    assert_works(&engine, &key, "nothing was recorded").await;
    engine.close().await;
}

/// 1,000 created keys hold a slot whatever the clock reads; a revoke or a recorded expiry frees
/// one. The refusal is retryable, and its hint says what frees a slot.
#[tokio::test]
async fn a_thousand_created_keys_hold_a_slot_whatever_the_clock_does() {
    let dir = tempfile::tempdir().unwrap();
    let clock = ManualClock::new(at(T));
    let (engine, admin) = open(dir.path(), clock.clone()).await;
    let first = create(&engine, &admin, &[Scope::Read], T + HOUR).await;
    // 998 more, expiring with it, straight into the table: through the engine each would be its
    // own durable transaction.
    db(dir.path())
        .execute_batch(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 998)
             INSERT INTO keys (id, environment_id, environment_token, scopes, digest, origin,
                               created_by, expires_at, created_at)
             SELECT printf('019a9f3e-7c2b-7d41-8a3e-%012x', i), k.environment_id,
                    k.environment_token, k.scopes, k.digest, k.origin, k.created_by,
                    k.expires_at, k.created_at
             FROM n, keys k WHERE k.seq = 2",
        )
        .unwrap();
    let full = async |case: &str| {
        let err = engine
            .create_key(&call(&admin), CreateKeyRequest::default())
            .await
            .expect_err(case);
        assert_eq!(
            (err.kind, err.message.as_str(), err.hint.as_deref()),
            (
                ErrorKind::ResourceExhausted,
                "key limit reached",
                Some("revoke keys, expired ones first")
            ),
            "{case}"
        );
        assert!(err.kind.retryable());
    };
    let last = create(&engine, &admin, &[Scope::Read], T + HOUR).await;
    full("the 1,001st").await;
    clock.set(at(T + HOUR));
    full("every created key expired by the clock").await;

    let req = RevokeKeyRequest {
        key_id: last[4..40].to_owned(),
    };
    engine.revoke_key(&call(&admin), req).await.unwrap();
    create(&engine, &admin, &[Scope::Read], T + 2 * HOUR).await;
    full("one slot freed, one taken").await;
    clock.set(at(T));
    full("the clock set back").await;
    let held = "SELECT count(*) FROM keys \
                WHERE origin = 'created' AND revoked_at IS NULL AND expiry_recorded = 0";
    let held: i64 = db(dir.path()).query_row(held, [], |r| r.get(0)).unwrap();
    assert_eq!(held, 1_000);

    clock.set(at(T + HOUR));
    assert_refused(&engine, &first, "expired").await;
    create(&engine, &admin, &[Scope::Read], T + 2 * HOUR).await;
    full("a recorded expiry's slot freed and taken").await;
    engine.close().await;
}
