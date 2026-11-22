//! Admission through the engine: each refusal, until the tenancy scenarios that list events
//! (ACC-020–024) can run.

use ironflowx_authz::mint;
use ironflowx_cell::ManualClock;
use ironflowx_core::{ErrorKind, Secret, Timestamp};
use ironflowx_operations::Engine;
use ironflowx_proto::{Call, OpError, WhoamiRequest};

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
    for (case, text) in [
        ("unknown", unknown.expose()),
        ("wrong secret", wrong_secret.expose()),
        ("another engine's", foreign.as_str()),
    ] {
        let err = refusal(engine, &key(text)).await;
        assert_eq!(
            (err.message.as_str(), err.hint),
            ("invalid API key", None),
            "{case}"
        );
    }

    for text in ["bad-key".to_owned(), format!("Bearer {bootstrap}")] {
        let err = refusal(engine, &key(&text)).await;
        assert_eq!(err.message, "invalid API key");
        let hint = err.hint.expect("a malformed key's hint names the format");
        assert!(hint.contains("ixk_<key-id>_<secret>"), "{hint}");
        assert!(!hint.contains(&text) && !err.message.contains(&text));
    }
    opened.engine.close().await;
    other.engine.close().await;
}
