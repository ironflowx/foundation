//! Seeds through `Engine::open` (D33): what each open reports, and that an open that fails
//! changes no seed (TEN-9). ACC-004–008, 013 and 014 cover the same rules as a host sees them.

use std::path::Path;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ironflowx_authz::mint;
use ironflowx_cell::{Ledger, ManualClock, OpenError};
use ironflowx_core::{Clock, EnvRef, ErrorKind, Secret, Timestamp};
use ironflowx_operations::{Engine, EngineError, Opened, SeedState};
use ironflowx_proto::{Call, ListEnvironmentsRequest, Operations, WhoamiRequest};

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

/// Whether `key` is accepted; if not, it must be refused as an invalid key.
async fn authenticates(engine: &Engine, key: &str) -> bool {
    let cx = Call::with_key(secret(key));
    let Err(err) = engine.whoami(&cx, WhoamiRequest {}).await else {
        return true;
    };
    assert_eq!(
        (err.kind, err.message.as_str(), err.hint),
        (ErrorKind::Unauthenticated, "invalid API key", None)
    );
    false
}

/// The error of an open that must fail, its message free of the seed and of its secret alone.
async fn refused(dir: &Path, seed: &str) -> EngineError {
    let err = open(dir, Some(seed)).await.err().expect("refused");
    for shown in [format!("{err}"), format!("{err:?}")] {
        assert!(!shown.contains(seed), "{shown}");
        assert!(!shown.contains(&seed[41..]), "{shown}");
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

/// A clock that, once armed, holds one reading, after letting some go: it says the reading began,
/// then waits for leave to finish it.
#[derive(Clone, Default)]
struct Held(Arc<Mutex<Option<Hook>>>);

struct Hook {
    skip: usize,
    reading: Sender<()>,
    go: Receiver<()>,
}

impl Clock for Held {
    fn now(&self) -> Timestamp {
        let mut armed = self.0.lock().unwrap();
        let hook = match &mut *armed {
            Some(hook) if hook.skip > 0 => {
                hook.skip -= 1;
                None
            }
            armed => armed.take(),
        };
        drop(armed);
        if let Some(hook) = hook {
            hook.reading.send(()).unwrap();
            let _ = hook.go.recv();
        }
        clock().now()
    }
}

/// Opens `dir` with `seed` and cancels the open inside the control-plane write that makes its
/// clock reading number `skip`, counting from 0. The root stays locked until that write ends;
/// then, how the next open, given no seed, ends.
async fn cancel_inside_a_write(dir: &Path, seed: &str, skip: usize) -> Result<Opened, EngineError> {
    let held = Held::default();
    let (reading, writing) = channel();
    let (go, wait) = channel();
    *held.0.lock().unwrap() = Some(Hook {
        skip,
        reading,
        go: wait,
    });
    let (root, seed) = (dir.to_owned(), Some(secret(seed)));
    let task = tokio::spawn(async move { Engine::open(&root, held, seed).await.map(drop) });
    while writing.try_recv().is_err() {
        assert!(!task.is_finished(), "the open ended without that reading");
        tokio::task::yield_now().await;
    }
    // The clock is read inside the transaction: the write lock is taken.
    let db = rusqlite::Connection::open(dir.join("control.db")).unwrap();
    db.busy_timeout(Duration::ZERO).unwrap();
    let busy = db.execute_batch("BEGIN IMMEDIATE").unwrap_err();
    assert_eq!(
        busy.sqlite_error_code(),
        Some(rusqlite::ErrorCode::DatabaseBusy)
    );
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());

    let err = open(dir, None).await.err().expect("refused");
    assert!(matches!(err, EngineError::Locked), "{err:?}");
    go.send(()).unwrap();
    loop {
        match open(dir, None).await {
            Err(EngineError::Locked) => tokio::task::yield_now().await,
            opened => break opened,
        }
    }
}

/// An open cancelled while it applies its seed keeps the root until the seed is applied or not,
/// so no later open runs beside it, its own seed then revoked by the cancelled one's.
#[tokio::test]
async fn a_cancelled_open_holds_the_root_until_its_seed_is_applied() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (seed(1, 1), seed(2, 2));
    open(dir.path(), Some(&a))
        .await
        .unwrap()
        .engine
        .close()
        .await;
    let opened = cancel_inside_a_write(dir.path(), &b, 0).await.unwrap();
    assert!(authenticates(&opened.engine, &b).await);
    assert!(!authenticates(&opened.engine, &a).await);
    opened.engine.close().await;
}

/// So does a cancelled first open, through the bootstrap's transaction and through the mark's:
/// the first leaves an unmarked bootstrap, which the next open finishes, the second a finished
/// one. Either way the seed stays the bootstrap key.
#[tokio::test]
async fn a_cancelled_first_open_holds_the_root_until_its_write_ends() {
    let a = seed(1, 1);
    for skip in [0, 1] {
        let dir = tempfile::tempdir().unwrap();
        let opened = cancel_inside_a_write(dir.path(), &a, skip).await.unwrap();
        assert!(authenticates(&opened.engine, &a).await);
        assert!(opened.minted.is_none() && !opened.lost_secret);
        opened.engine.close().await;
    }
}
