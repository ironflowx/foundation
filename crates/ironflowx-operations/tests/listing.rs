//! Listing and revoking keys through the engine (D35): order, status, pages and cursors, and
//! what a revoke changes.

use std::collections::BTreeSet;
use std::num::NonZeroU32;

use ironflowx_cell::ManualClock;
use ironflowx_core::{ErrorKind, KeyOrigin, Scope, Secret, Timestamp};
use ironflowx_operations::Engine;
use ironflowx_proto::{
    Call, CreateKeyRequest, KeyPage, KeyStatus, ListKeysRequest, OpError, Operations,
    RevokeKeyRequest, WhoamiRequest,
};

const T: i64 = 1_800_000_000_000;
const HOUR: i64 = 3_600_000;

const fn at(ms: i64) -> Timestamp {
    Timestamp::from_millis(ms)
}

async fn open(dir: &std::path::Path, clock: &ManualClock) -> (Engine, Call) {
    let opened = Engine::open(dir, clock.clone(), None).await.unwrap();
    (opened.engine, Call::with_key(opened.minted.unwrap()))
}

async fn create(engine: &Engine, cx: &Call, label: &str, expires_at: Option<i64>) -> String {
    let req = CreateKeyRequest {
        scopes: BTreeSet::from([Scope::Read]),
        expires_at: expires_at.map(at),
        label: Some(label.into()),
    };
    let created = engine.create_key(cx, req).await.unwrap();
    created.info.key_id.to_string()
}

async fn list(engine: &Engine, cx: &Call, limit: Option<u32>, cursor: Option<&str>) -> KeyPage {
    let req = ListKeysRequest {
        limit: limit.and_then(NonZeroU32::new),
        cursor: cursor.map(str::to_owned),
    };
    engine.list_keys(cx, req).await.unwrap()
}

async fn revoke(engine: &Engine, cx: &Call, key_id: &str) -> Result<(), OpError> {
    let req = RevokeKeyRequest {
        key_id: key_id.into(),
    };
    engine.revoke_key(cx, req).await
}

#[tokio::test]
async fn keys_list_newest_first_with_their_origin_creator_and_status() {
    let dir = tempfile::tempdir().unwrap();
    let clock = ManualClock::new(at(T));
    let (engine, admin) = open(dir.path(), &clock).await;
    let admin_id = engine
        .whoami(&admin, WhoamiRequest {})
        .await
        .unwrap()
        .key_id;
    let a = create(&engine, &admin, "A", None).await;
    let b = create(&engine, &admin, "B", None).await;
    let c = create(&engine, &admin, "C", Some(T + HOUR)).await;
    revoke(&engine, &admin, &b).await.unwrap();
    clock.set(at(T + 2 * HOUR));

    let page = list(&engine, &admin, None, None).await;
    assert_eq!(page.next, None);
    let shown: Vec<_> = page
        .keys
        .iter()
        .map(|k| {
            (
                k.key_id.to_string(),
                k.label.as_deref(),
                k.status,
                k.origin,
                k.created_by,
            )
        })
        .collect();
    let by = Some(admin_id);
    assert_eq!(
        shown,
        [
            (c, Some("C"), KeyStatus::Expired, KeyOrigin::Created, by),
            (b, Some("B"), KeyStatus::Revoked, KeyOrigin::Created, by),
            (a, Some("A"), KeyStatus::Active, KeyOrigin::Created, by),
            (
                admin_id.to_string(),
                None,
                KeyStatus::Active,
                KeyOrigin::Minted,
                None
            ),
        ]
    );
    let bootstrap = &page.keys[3];
    assert_eq!(
        bootstrap.scopes,
        BTreeSet::from([Scope::Read, Scope::Write, Scope::Admin, Scope::Execute])
    );
    assert_eq!((bootstrap.created_at, bootstrap.expires_at), (at(T), None));
    engine.close().await;
}

/// A page holds 100 keys by default and 1,000 at most; cursors walk every key once and carry
/// the incarnation token, so a cursor that does not decode, or names another, is `Invalid`.
#[tokio::test]
async fn keys_list_a_page_at_a_time() {
    let dir = tempfile::tempdir().unwrap();
    let clock = ManualClock::new(at(T));
    let (engine, admin) = open(dir.path(), &clock).await;
    // 1,500 more keys in one transaction, straight into the table: through the engine each
    // would be its own durable transaction.
    let db = rusqlite::Connection::open(dir.path().join("control.db")).unwrap();
    db.execute_batch(
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 1500)
         INSERT INTO keys (id, environment_id, environment_token, scopes, digest, origin,
                           created_by, created_at)
         SELECT printf('019a9f3e-7c2b-7d41-8a3e-%012x', i), k.environment_id,
                k.environment_token, 'read', k.digest, 'created', k.id, k.created_at
         FROM n, keys k WHERE k.seq = 1",
    )
    .unwrap();

    let first = list(&engine, &admin, None, None).await;
    assert_eq!(first.keys.len(), 100);
    let clamped = list(&engine, &admin, Some(5_000), None).await;
    assert_eq!(clamped.keys.len(), 1_000);

    let mut seen = BTreeSet::new();
    let mut cursor = None;
    loop {
        let page = list(&engine, &admin, Some(400), cursor.as_deref()).await;
        for key in &page.keys {
            assert!(
                seen.insert(key.key_id.to_string()),
                "listed twice: {}",
                key.key_id
            );
        }
        match page.next {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    assert_eq!(seen.len(), 1_501);

    let next = first.next.unwrap();
    let (token, seq) = next.split_once('.').unwrap();
    let other_token = format!(
        "{}.{seq}",
        if token.starts_with('0') { "1" } else { "0" }.repeat(32)
    );
    // Past what a rowid can be, though a u64 holds it.
    let past_rowids = format!("{token}.{}", i64::MAX as u64 + 1);
    for bad in [
        "",
        "x",
        "1",
        &format!("{token}.x"),
        &format!("{token}.0{seq}"),
        &format!("{token}.-1"),
        &other_token,
        &past_rowids,
        &format!("{token}.{}", u64::MAX),
    ] {
        let req = ListKeysRequest {
            limit: None,
            cursor: Some(bad.to_owned()),
        };
        let err = engine.list_keys(&admin, req).await.expect_err(bad);
        assert_eq!(err.kind, ErrorKind::Invalid, "{bad:?}");
    }
    engine.close().await;
}

#[tokio::test]
async fn a_revoked_key_is_refused_from_its_next_call() {
    let dir = tempfile::tempdir().unwrap();
    let clock = ManualClock::new(at(T));
    let (engine, admin) = open(dir.path(), &clock).await;
    let req = CreateKeyRequest {
        scopes: BTreeSet::from([Scope::Read]),
        ..CreateKeyRequest::default()
    };
    let created = engine.create_key(&admin, req).await.unwrap();
    let k = Call::with_key(Secret::new(created.key.expose().to_owned()));
    let id = created.info.key_id.to_string();
    engine
        .whoami(&k, WhoamiRequest {})
        .await
        .expect("before the revoke");

    revoke(&engine, &admin, &id).await.unwrap();
    let err = engine
        .whoami(&k, WhoamiRequest {})
        .await
        .expect_err("revoked");
    assert_eq!(
        (err.kind, err.message.as_str(), err.hint),
        (ErrorKind::Unauthenticated, "invalid API key", None)
    );
    revoke(&engine, &admin, &id)
        .await
        .expect("revoking a revoked key");

    for unknown in ["not-a-key-id", "019a9f3e-7c2b-7d41-8a3e-5f6b7c8d9e0f"] {
        let err = revoke(&engine, &admin, unknown).await.expect_err(unknown);
        assert_eq!(
            (err.kind, err.message.as_str()),
            (ErrorKind::NotFound, "key not found"),
            "{unknown}"
        );
    }
    engine.close().await;
}
