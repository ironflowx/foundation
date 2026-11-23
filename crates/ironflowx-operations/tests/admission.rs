//! Admission through the engine: each refusal, byte for byte (TEN-7), and a key row of another
//! environment or incarnation (TEN-10). ACC-020–024 cover the same refusals as a caller sees them.

use std::collections::BTreeSet;

use ironflowx_authz::{KeyRecord, mint, parse, verify};
use ironflowx_cell::ManualClock;
use ironflowx_core::{Clock, EnvRef, ErrorKind, Scope, Secret, Timestamp};
use ironflowx_operations::Engine;
use ironflowx_proto::{Call, CreateKeyRequest, OpError, RevokeKeyRequest, WhoamiRequest};

fn clock() -> ManualClock {
    ManualClock::new(Timestamp::from_millis(1_800_000_000_000))
}

fn key(text: &str) -> Call {
    Call::with_key(Secret::new(text.to_owned()))
}

async fn refusal(engine: &Engine, cx: &Call) -> OpError {
    let err = engine
        .whoami(cx, WhoamiRequest {})
        .await
        .expect_err("refused");
    assert_eq!(err.kind, ErrorKind::Unauthenticated, "{err:?}");
    assert!(!err.kind.retryable());
    err
}

#[tokio::test]
async fn every_bad_key_is_refused_and_only_a_malformed_one_hints() {
    let dir = tempfile::tempdir().unwrap();
    let other_dir = tempfile::tempdir().unwrap();
    let opened = Engine::open(dir.path(), clock()).await.unwrap();
    let other = Engine::open(other_dir.path(), clock()).await.unwrap();
    let engine = &opened.engine;
    let bootstrap = opened.minted.as_ref().unwrap().expose().to_owned();
    let foreign = other.minted.as_ref().unwrap().expose().to_owned();
    engine
        .whoami(&key(&bootstrap), WhoamiRequest {})
        .await
        .expect("the bootstrap key");

    for cx in [Call::anonymous(), key("")] {
        let err = refusal(engine, &cx).await;
        assert_eq!(
            (err.message.as_str(), err.hint),
            ("authentication required", None)
        );
    }

    let unknown = mint(
        "019a9f3e-7c2b-7d41-8a3e-5f6b7c8d9e0f".parse().unwrap(),
        [1; 32],
    )
    .0;
    let id = &bootstrap[4..40];
    let wrong_secret = mint(id.parse().unwrap(), [2; 32]).0;
    let first = refusal(engine, &key(unknown.expose())).await;
    assert_eq!(
        (first.message.as_str(), &first.hint),
        ("invalid API key", &None)
    );
    // One answer, so no refusal tells which keys exist (D30).
    for (case, text) in [
        ("wrong secret", wrong_secret.expose()),
        ("another engine's", foreign.as_str()),
    ] {
        assert_eq!(refusal(engine, &key(text)).await, first, "{case}");
    }

    for text in ["bad-key".to_owned(), format!("Bearer {bootstrap}")] {
        let err = refusal(engine, &key(&text)).await;
        assert_eq!(err.message, "invalid API key");
        let hint = err
            .hint
            .as_deref()
            .expect("a malformed key's hint names the format");
        assert!(hint.contains("ixk_<key-id>_<secret>"), "{hint}");
        let shown = format!("{err:?}");
        for part in [text.as_str(), &bootstrap, &bootstrap[41..]] {
            assert!(!shown.contains(part), "{shown}");
        }
    }
    opened.engine.close().await;
    other.engine.close().await;
}

/// The five kinds of bad key get one answer, byte for byte, and the same one when recording an
/// expiry fails; a wrong-secret or revoked key that has also expired is no different (TEN-7).
#[tokio::test]
async fn the_five_refusals_are_one_answer_whether_or_not_an_expiry_is_recorded() {
    const HOUR: i64 = 3_600_000;
    let (dir, other_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let clock = clock();
    let t = clock.now().as_millis();
    let opened = Engine::open(dir.path(), clock.clone()).await.unwrap();
    let other = Engine::open(other_dir.path(), clock.clone()).await.unwrap();
    let engine = &opened.engine;
    let admin = key(opened.minted.as_ref().unwrap().expose());
    let mut made = Vec::new();
    for _ in 0..4 {
        let req = CreateKeyRequest {
            expires_at: Some(Timestamp::from_millis(t + HOUR)),
            ..CreateKeyRequest::default()
        };
        made.push(engine.create_key(&admin, req).await.unwrap());
    }
    let revoke = async |id: String| {
        let req = RevokeKeyRequest { key_id: id };
        engine.revoke_key(&admin, req).await.unwrap();
    };
    revoke(made[2].info.key_id.to_string()).await;
    let wrong = mint(made[3].info.key_id, [2; 32]).0;
    let unknown = mint(
        "019a9f3e-7c2b-7d41-8a3e-5f6b7c8d9e0f".parse().unwrap(),
        [1; 32],
    )
    .0;
    let recorded = || -> i64 {
        let db = rusqlite::Connection::open(dir.path().join("control.db")).unwrap();
        let sql = "SELECT count(*) FROM keys WHERE expiry_recorded";
        db.query_row(sql, [], |r| r.get(0)).unwrap()
    };
    let expected = OpError {
        kind: ErrorKind::Unauthenticated,
        message: "invalid API key".into(),
        hint: None,
    };

    clock.set(Timestamp::from_millis(t + HOUR));
    let bad = [
        ("expired", made[0].key.expose()),
        ("unknown", unknown.expose()),
        ("wrong secret", wrong.expose()),
        ("revoked", made[2].key.expose()),
        ("foreign", other.minted.as_ref().unwrap().expose()),
    ];
    let db = rusqlite::Connection::open(dir.path().join("control.db")).unwrap();
    db.execute_batch(
        "CREATE TRIGGER fail BEFORE UPDATE ON keys BEGIN SELECT RAISE(ABORT, 'injected'); END;",
    )
    .unwrap();
    for (case, text) in bad {
        let got = refusal(engine, &key(text)).await;
        assert_eq!(got, expected, "{case}, recording failing");
    }
    assert_eq!(recorded(), 0, "the recording did fail");
    db.execute_batch("DROP TRIGGER fail").unwrap();
    // The expired key twice: newly expired, then recorded.
    for (case, text) in bad.into_iter().chain([bad[0]]) {
        assert_eq!(refusal(engine, &key(text)).await, expected, "{case}");
    }
    assert_eq!(recorded(), 1, "the expired key alone");
    opened.engine.close().await;
    other.engine.close().await;
}

#[tokio::test]
async fn a_key_row_of_another_environment_or_incarnation_is_refused_by_lookup_and_verify() {
    let dir = tempfile::tempdir().unwrap();
    let opened = Engine::open(dir.path(), clock()).await.unwrap();
    let bootstrap = opened.minted.as_ref().unwrap().expose().to_owned();
    let unknown = mint(
        "019a9f3e-7c2b-7d41-8a3e-5f6b7c8d9e0f".parse().unwrap(),
        [1; 32],
    )
    .0;
    let unknown = refusal(&opened.engine, &key(unknown.expose())).await;
    opened.engine.close().await;

    let db = rusqlite::Connection::open(dir.path().join("control.db")).unwrap();
    // The planted row names an environment this file does not hold.
    db.pragma_update(None, "foreign_keys", false).unwrap();
    let (env_id, token): (String, String) = db
        .query_row(
            "SELECT environment_id, environment_token FROM keys",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    let engine_env: EnvRef = format!("{env_id}.{token}").parse().unwrap();
    let other_token = if token == format!("{:032x}", 1) { 2 } else { 1 };
    let (other_id, other_token) = (
        "019a9f3e-7c2b-7d41-8a3e-000000000002".to_owned(),
        format!("{other_token:032x}"),
    );
    let parts = parse(&Secret::new(bootstrap.clone())).unwrap();

    // The row keeps its matching digest; only its environment or its incarnation moves.
    for (case, row_id, row_token) in [
        ("another environment", &other_id, &token),
        ("another incarnation", &env_id, &other_token),
        ("its own", &env_id, &token),
    ] {
        db.execute(
            "UPDATE keys SET environment_id = ?1, environment_token = ?2",
            [row_id, row_token],
        )
        .unwrap();
        let engine = Engine::open(dir.path(), clock()).await.unwrap().engine;
        let answer = engine.whoami(&key(&bootstrap), WhoamiRequest {}).await;
        let record = KeyRecord {
            id: parts.id,
            env: format!("{row_id}.{row_token}").parse().unwrap(),
            scopes: BTreeSet::from([Scope::Admin]),
            digest: parts.digest.clone(),
            expires_at: None,
            expiry_recorded: false,
            revoked_at: None,
        };
        let verified = verify(&parts, Some(&record), &engine_env, clock().now());
        if case == "its own" {
            // Moved back, the key works: the refusals above came from the move alone.
            assert!(answer.is_ok() && verified.is_ok(), "{case}");
        } else {
            assert_eq!(answer.err(), Some(unknown.clone()), "{case}: lookup");
            assert!(verified.is_err(), "{case}: verify");
        }
        engine.close().await;
    }
}
