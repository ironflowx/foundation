//! Seeds through `Engine::open` (D33): what each open reports, and that an open that fails
//! changes no seed (TEN-9). ACC-004–008, 013 and 014 cover the same rules as a host sees them.

use std::path::Path;

use ironflowx_authz::mint;
use ironflowx_cell::{Ledger, ManualClock, OpenError};
use ironflowx_core::{EnvRef, Secret, Timestamp};
use ironflowx_operations::{Engine, EngineError, Opened, SeedState};
use ironflowx_proto::{Call, ListEnvironmentsRequest, WhoamiRequest};

fn clock() -> ManualClock {
    ManualClock::new(Timestamp::from_millis(1_800_000_000_000))
}

/// A seed's text: a well-formed key, its ID from `n` and its secret from `secret`.
fn seed(n: u8, secret: u8) -> String {
    let id = format!("019a9f3e-7c2b-7d41-8a3e-2000000000{n:02x}");
    mint(id.parse().unwrap(), [secret; 32])
        .0
        .expose()
        .to_owned()
}

fn secret(text: &str) -> Secret {
    Secret::new(text.to_owned())
}

async fn open(dir: &Path, seed: Option<&str>) -> Result<Opened, EngineError> {
    Engine::open(dir, clock(), seed.map(secret)).await
}

async fn authenticates(engine: &Engine, key: &str) -> bool {
    let cx = Call::with_key(secret(key));
    engine.whoami(&cx, WhoamiRequest {}).await.is_ok()
}

/// The error of an open that must fail, its message free of the seed.
async fn refused(dir: &Path, seed: &str) -> EngineError {
    let err = open(dir, Some(seed)).await.err().expect("refused");
    for shown in [format!("{err}"), format!("{err:?}")] {
        assert!(!shown.contains(seed), "{shown}");
    }
    err
}

#[tokio::test]
async fn each_open_reports_its_seed_and_returns_a_key_only_when_it_minted_one() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (seed(1, 1), seed(2, 2));
    for (seed, reported, works, refused) in [
        (Some(&a), Some(SeedState::Current), &a, None),
        (None, None, &a, None),
        (Some(&a), Some(SeedState::Current), &a, None),
        (Some(&b), Some(SeedState::Current), &b, Some(&a)),
        (Some(&a), Some(SeedState::Revoked), &b, Some(&a)),
        (None, None, &b, Some(&a)),
    ] {
        let opened = open(dir.path(), seed.map(String::as_str)).await.unwrap();
        assert!(opened.minted.is_none());
        assert_eq!(opened.seed, reported);
        assert!(authenticates(&opened.engine, works).await);
        if let Some(old) = refused {
            assert!(!authenticates(&opened.engine, old).await);
        }
        opened.engine.close().await;
    }

    let dir = tempfile::tempdir().unwrap();
    let opened = open(dir.path(), None).await.unwrap();
    assert!(opened.minted.is_some());
    assert_eq!(opened.seed, None);
    opened.engine.close().await;
}

/// A malformed seed is refused before anything else, so nothing is read or created: neither a
/// missing root, nor a root another engine holds, nor one whose control plane is not a database.
#[tokio::test]
async fn a_malformed_seed_is_refused_before_anything_is_read() {
    let dir = tempfile::tempdir().unwrap();
    let good = seed(1, 1);
    let malformed = [
        String::new(),
        "not-a-key".to_owned(),
        good[..83].to_owned(),
        format!("{good}A"),
        good.replace("019a9f3e", "019A9F3E"),
        good.replace("-7d41-", "-4d41-"),
    ];
    let missing = dir.path().join("missing");
    let broken = dir.path().join("broken");
    std::fs::create_dir(&broken).unwrap();
    std::fs::write(broken.join("control.db"), "not a database").unwrap();
    let held = dir.path().join("held");
    let opened = open(&held, Some(&good)).await.unwrap();
    for root in [&missing, &broken, &held] {
        for text in &malformed {
            let err = open(root, Some(text)).await.err().expect("refused");
            assert!(matches!(err, EngineError::MalformedSeed), "{err:?}");
            let shown = format!("{err} {err:?}");
            assert!(shown.contains("ixk_<key-id>_<secret>"), "{shown}");
            assert!(text.is_empty() || !shown.contains(text.as_str()), "{shown}");
        }
    }
    assert!(!missing.exists());
    let left: Vec<_> = std::fs::read_dir(&broken).unwrap().collect();
    assert_eq!(left.len(), 1, "{left:?}");
    assert!(authenticates(&opened.engine, &good).await);
    opened.engine.close().await;
}

/// A seed is applied only once the ledger has opened: with the ledger held by another opener,
/// the open fails and the old seed still authenticates, the new one not (TEN-9).
#[tokio::test]
async fn a_failed_ledger_open_changes_no_seed() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (seed(1, 1), seed(2, 2));
    let opened = open(dir.path(), Some(&a)).await.unwrap();
    let cx = Call::with_key(secret(&a));
    let env: EnvRef = opened
        .engine
        .list_environments(&cx, ListEnvironmentsRequest {})
        .await
        .unwrap()[0]
        .env;
    opened.engine.close().await;

    let ledger = dir.path().join("ledgers").join(env.id().to_string());
    let holder = Ledger::open(&ledger, env, clock()).await.unwrap();
    let err = refused(dir.path(), &b).await;
    assert!(
        matches!(err, EngineError::Ledger(OpenError::Locked)),
        "{err:?}"
    );
    holder.close().await;

    let opened = open(dir.path(), None).await.unwrap();
    assert!(authenticates(&opened.engine, &a).await);
    assert!(!authenticates(&opened.engine, &b).await);
    opened.engine.close().await;
}

/// A seed refused as a conflict, or that the control plane fails to apply, changes no key, and
/// the failed open lets go of the directory: the next one opens at once (TEN-9).
#[tokio::test]
async fn a_seed_that_is_not_applied_changes_no_key_and_releases_the_directory() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (seed(1, 1), seed(2, 2));
    let opened = open(dir.path(), Some(&a)).await.unwrap();
    opened.engine.close().await;

    let other_secret = seed(1, 9);
    let err = refused(dir.path(), &other_secret).await;
    assert!(matches!(err, EngineError::SeedConflict), "{err:?}");

    let db = rusqlite::Connection::open(dir.path().join("control.db")).unwrap();
    db.execute_batch(
        "CREATE TRIGGER fail BEFORE INSERT ON keys BEGIN SELECT RAISE(ABORT, 'injected'); END;",
    )
    .unwrap();
    let err = refused(dir.path(), &b).await;
    assert!(matches!(err, EngineError::Store(_)), "{err:?}");
    db.execute_batch("DROP TRIGGER fail").unwrap();

    let opened = open(dir.path(), None).await.unwrap();
    assert!(authenticates(&opened.engine, &a).await);
    for not_applied in [&other_secret, &b] {
        assert!(!authenticates(&opened.engine, not_applied).await);
    }
    opened.engine.close().await;
}
