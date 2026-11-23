//! `recover_admin_key` (D34): whoever holds the data directory gets back in, and a key recovered
//! while an engine opens, finishes or fails is never undone (TEN-14).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use ironflowx_authz::{KeyParts, mint, parse};
use ironflowx_cell::{Ledger, ManualClock, new_env_ref};
use ironflowx_core::{Clock, EnvRef, ErrorKind, KeyOrigin, Scope, Secret, Timestamp};
use ironflowx_operations::{Engine, EngineError, Opened, RecoverError, recover_admin_key};
use ironflowx_proto::{Call, CreateKeyRequest, Operations, RevokeKeyRequest, WhoamiRequest};
use ironflowx_store::{Bootstrap, BootstrapState, ControlPlane, StoreError};

const NOW: i64 = 1_800_000_000_000;

fn clock() -> ManualClock {
    ManualClock::new(Timestamp::from_millis(NOW))
}

fn key(n: u8) -> String {
    let id = format!("019a9f3e-7c2b-7d41-8a3e-2000000000{n:02x}");
    let (secret, _) = mint(id.parse().unwrap(), [n; 32]);
    secret.expose().to_owned()
}

fn secret(text: &str) -> Secret {
    Secret::new(text.to_owned())
}

fn parts(text: &str) -> KeyParts {
    parse(&secret(text)).unwrap()
}

async fn open(root: &Path, seed: Option<&str>) -> Opened {
    let seed = seed.map(secret);
    Engine::open(root, clock(), seed).await.expect("opens")
}

fn recover(root: &Path) -> Result<Secret, RecoverError> {
    recover_admin_key(root, clock())
}

fn not_ready(got: Result<Secret, RecoverError>) {
    match got {
        Ok(_) => panic!("recovered"),
        Err(e) => assert!(matches!(e, RecoverError::NotReady), "{e:?}"),
    }
}

/// Whether `key` is accepted, and then as a key of all three scopes; if not, as an invalid one.
async fn admits(engine: &Engine, key: &str) -> bool {
    let every = BTreeSet::from([Scope::Read, Scope::Write, Scope::Admin]);
    let cx = Call::with_key(secret(key));
    match engine.whoami(&cx, WhoamiRequest {}).await {
        Ok(me) => {
            assert_eq!(me.scopes, every);
            true
        }
        Err(err) => {
            assert_eq!(err.kind, ErrorKind::Unauthenticated);
            assert_eq!(err.message, "invalid API key");
            false
        }
    }
}

fn control(root: &Path) -> PathBuf {
    root.join("control.db")
}

fn state(root: &Path) -> BootstrapState {
    ControlPlane::bootstrap_state(&control(root)).unwrap()
}

/// Every key's ID and origin, oldest first.
fn keys(root: &Path) -> Vec<(String, KeyOrigin)> {
    let (BootstrapState::Unmarked { env } | BootstrapState::Marked { env }) = state(root) else {
        panic!("not bootstrapped");
    };
    let cp = ControlPlane::open(&control(root)).unwrap();
    let rows = cp.list_keys(&env, None, 10).unwrap();
    let rows = rows.into_iter().rev();
    rows.map(|r| (r.record.id.to_string(), r.origin)).collect()
}

fn id(text: &str) -> String {
    parts(text).id.to_string()
}

/// A root whose first open, with a seed, died after its rename and before its mark.
fn died(root: &Path, seed: &str) -> EnvRef {
    let env = new_env_ref();
    let bootstrap = Bootstrap {
        env,
        tenant: "019a9f3e-7c2b-7d41-8a3e-100000000001".parse().unwrap(),
        project: "019a9f3e-7c2b-7d41-8a3e-100000000002".parse().unwrap(),
        key: parts(seed),
        by_seed: true,
    };
    let cp = ControlPlane::open(&control(root)).unwrap();
    cp.ensure_bootstrap(&bootstrap, &clock()).unwrap();
    let at = root.join("ledgers").join(env.id().to_string());
    drop(Ledger::open_blocking(&at, env, clock()).unwrap());
    env
}

/// The last admin key, its own included, may be revoked: a key recovered beside the engine is
/// accepted on its next call, with all three scopes. Again after a restart.
#[tokio::test]
async fn a_key_recovered_beside_an_engine_works_on_its_next_call() {
    let dir = tempfile::tempdir().unwrap();
    let opened = open(dir.path(), None).await;
    let minted = opened.minted.unwrap().expose().to_owned();
    let cx = Call::with_key(secret(&minted));
    let req = RevokeKeyRequest {
        key_id: id(&minted),
    };
    opened.engine.revoke_key(&cx, req).await.unwrap();
    assert!(!admits(&opened.engine, &minted).await);

    let first = recover(dir.path()).unwrap();
    assert!(admits(&opened.engine, first.expose()).await);
    let cx = Call::with_key(secret(first.expose()));
    let created = opened.engine.create_key(&cx, CreateKeyRequest::default());
    created.await.expect("an admin key creates a key");
    let second = recover(dir.path()).unwrap();
    assert!(admits(&opened.engine, second.expose()).await);
    opened.engine.close().await;

    let third = recover(dir.path()).unwrap();
    let opened = open(dir.path(), None).await;
    assert!(opened.minted.is_none() && !opened.lost_secret);
    assert!(!admits(&opened.engine, &minted).await);
    for recovered in [first, second, third] {
        assert!(admits(&opened.engine, recovered.expose()).await);
    }
    opened.engine.close().await;
}

/// Not ready, with nothing created: a missing root, a bare ledger's, and an empty `control.db`,
/// which a crash before its schema leaves. An unmarked bootstrap is not ready until it finishes.
#[tokio::test]
async fn a_root_no_open_finished_is_not_ready_and_gains_no_file() {
    let dir = tempfile::tempdir().unwrap();
    let names = |root: &Path| {
        let entries = std::fs::read_dir(root).unwrap();
        let mut names: Vec<_> = entries.map(|e| e.unwrap().file_name()).collect();
        names.sort();
        names
    };
    let missing = dir.path().join("missing");
    not_ready(recover(&missing));
    assert!(!missing.exists());

    let bare = dir.path().join("bare");
    let ledger = Ledger::open(&bare, new_env_ref(), clock()).await.unwrap();
    ledger.close().await;
    let before = names(&bare);
    not_ready(recover(&bare));
    assert_eq!(names(&bare), before);

    let empty = dir.path().join("empty");
    std::fs::create_dir(&empty).unwrap();
    std::fs::write(control(&empty), "").unwrap();
    not_ready(recover(&empty));
    assert_eq!(names(&empty), ["control.db"]);
    assert_eq!(std::fs::read(control(&empty)).unwrap(), b"");

    let unmarked = dir.path().join("unmarked");
    std::fs::create_dir(&unmarked).unwrap();
    let a = key(1);
    let env = died(&unmarked, &a);
    not_ready(recover(&unmarked));
    assert_eq!(state(&unmarked), BootstrapState::Unmarked { env });
    assert_eq!(keys(&unmarked), [(id(&a), KeyOrigin::Seed)]);
    open(&unmarked, None).await.engine.close().await;
    let recovered = recover(&unmarked).expect("finished");
    let after = [
        (id(&a), KeyOrigin::Seed),
        (id(recovered.expose()), KeyOrigin::Recovered),
    ];
    assert_eq!(keys(&unmarked), after);
}

/// A control plane in another format, or that is none, is an error, and not "not ready".
#[tokio::test]
async fn a_control_plane_that_cannot_be_read_is_a_store_error() {
    let dir = tempfile::tempdir().unwrap();
    open(dir.path(), None).await.engine.close().await;
    let db = rusqlite::Connection::open(control(dir.path())).unwrap();
    db.execute_batch("UPDATE meta SET format = 2").unwrap();
    drop(db);
    let err = recover(dir.path()).err().expect("another format");
    let format = matches!(err, RecoverError::Store(StoreError::Format));
    assert!(format, "{err:?}");
    std::fs::write(control(dir.path()), "not a database").unwrap();
    let err = recover(dir.path()).err().expect("no control plane");
    let failed = matches!(err, RecoverError::Store(StoreError::Failed(_)));
    assert!(failed, "{err:?}");
}

type Act = Box<dyn FnOnce() + Send>;

/// A clock that acts at one reading, after letting `skip` go.
#[derive(Clone)]
struct OnReading(Arc<Mutex<Option<(usize, Act)>>>);

impl OnReading {
    fn new(skip: usize, act: impl FnOnce() + Send + 'static) -> Self {
        Self(Arc::new(Mutex::new(Some((skip, Box::new(act))))))
    }
}

impl Clock for OnReading {
    fn now(&self) -> Timestamp {
        let mut armed = self.0.lock().unwrap();
        let act = match armed.take() {
            Some((0, act)) => Some(act),
            Some((skip, act)) => {
                *armed = Some((skip - 1, act));
                None
            }
            None => None,
        };
        drop(armed);
        if let Some(act) = act {
            act();
        }
        Timestamp::from_millis(NOW)
    }
}

/// A clock that acts once, as the first ledger's writer ends: the staging ledger's, its bootstrap
/// committed and not marked, and no write lock held.
#[derive(Clone)]
struct OnWriterEnd(Arc<Mutex<Option<Act>>>);

impl Clock for OnWriterEnd {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(NOW)
    }
}

impl Drop for OnWriterEnd {
    fn drop(&mut self) {
        if std::thread::current().name() == Some("ironflowx-ledger-writer") {
            let act = self.0.lock().unwrap().take();
            if let Some(act) = act {
                act();
            }
        }
    }
}

/// A barrier: the one who waits says it is there, then waits for leave.
fn barrier() -> (impl FnOnce() + Send + 'static, Receiver<()>, Sender<()>) {
    let (tell, there) = channel();
    let (go, wait) = channel::<()>();
    let hold = move || {
        tell.send(()).unwrap();
        wait.recv_timeout(Duration::from_secs(4)).expect("leave");
    };
    (hold, there, go)
}

/// Waits off the runtime's thread for the open's blocking step to say it is there.
async fn arrives(there: &Receiver<()>, opening: &tokio::task::JoinHandle<Outcome>) {
    while there.try_recv().is_err() {
        assert!(!opening.is_finished(), "the open ended without stopping");
        tokio::task::yield_now().await;
    }
}

type Outcome = Result<Opened, EngineError>;

fn opening(
    root: &Path,
    clock: impl Clock + Clone + Send + Sync + 'static,
    seed: &str,
) -> tokio::task::JoinHandle<Outcome> {
    let (root, seed) = (root.to_owned(), Some(secret(seed)));
    tokio::spawn(async move { Engine::open(&root, clock, seed).await })
}

fn recovering(
    root: &Path,
    clock: impl Clock + Send + 'static,
) -> JoinHandle<Result<Secret, RecoverError>> {
    let root = root.to_owned();
    std::thread::spawn(move || recover_admin_key(&root, clock))
}

/// The open failed at its rename and undid its bootstrap: no key is left.
fn undone(root: &Path, opened: Outcome) {
    let err = opened.err().expect("the rename fails");
    assert!(matches!(err, EngineError::Io(_)), "{err:?}");
    assert_eq!(state(root), BootstrapState::Unbootstrapped);
    let db = rusqlite::Connection::open(control(root)).unwrap();
    let count = "SELECT count(*) FROM keys";
    let rows: u32 = db.query_row(count, [], |r| r.get(0)).unwrap();
    assert_eq!(rows, 0);
}

/// The recovery first: inside a bootstrapping open, after its transaction and before its mark, it
/// is refused and stores nothing, whether the open then marks or undoes its bootstrap.
#[tokio::test]
async fn a_recovery_before_the_mark_is_refused_and_stores_nothing() {
    let a = key(1);
    for fails in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let (tell, got) = channel();
        let root = dir.path().to_owned();
        let stop = OnWriterEnd(Arc::new(Mutex::new(Some(Box::new(move || {
            let unmarked = matches!(state(&root), BootstrapState::Unmarked { .. });
            tell.send((unmarked, recover(&root))).unwrap();
            if fails {
                let staging = root.join("ledgers/.staging");
                std::fs::rename(staging, root.join("away")).unwrap();
            }
        })))));
        let opened = opening(dir.path(), stop, &a).await.unwrap();
        let (unmarked, recovery) = got.recv().unwrap();
        assert!(unmarked, "stopped before its mark");
        not_ready(recovery);
        if fails {
            undone(dir.path(), opened);
        } else {
            opened.unwrap().engine.close().await;
            assert_eq!(keys(dir.path()), [(id(&a), KeyOrigin::Seed)]);
        }
    }
}

/// The recovery first, its transaction open when the open comes to write: the mark, or the undo,
/// waits for it. It saw no mark, so it is refused, and stores nothing.
#[tokio::test]
async fn a_recovery_whose_transaction_precedes_the_mark_or_the_undo_is_refused() {
    let a = key(1);
    for fails in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let (stop_open, stopped, go_open) = barrier();
        let root = dir.path().to_owned();
        let stop = OnWriterEnd(Arc::new(Mutex::new(Some(Box::new(move || {
            stop_open();
            if fails {
                let staging = root.join("ledgers/.staging");
                std::fs::rename(staging, root.join("away")).unwrap();
            }
        })))));
        let open = opening(dir.path(), stop, &a);
        arrives(&stopped, &open).await;
        let (hold, inside, go) = barrier();
        let recovery = recovering(dir.path(), OnReading::new(0, hold));
        inside.recv().unwrap();
        go_open.send(()).unwrap();
        // Long enough for the open to reach its mark or its undo, which must wait.
        let until = Instant::now() + Duration::from_millis(200);
        while Instant::now() < until {
            assert!(!open.is_finished(), "the open wrote beside the recovery");
            tokio::task::yield_now().await;
        }
        assert!(matches!(state(dir.path()), BootstrapState::Unmarked { .. }));
        go.send(()).unwrap();
        not_ready(recovery.join().unwrap());
        let opened = open.await.unwrap();
        if fails {
            undone(dir.path(), opened);
        } else {
            opened.unwrap().engine.close().await;
            assert_eq!(keys(dir.path()), [(id(&a), KeyOrigin::Seed)]);
        }
    }
}

/// The mark first, a bootstrapping or a finishing open's: a recovery that starts inside its
/// transaction waits for it, lands after it, and its row survives.
#[tokio::test]
async fn a_recovery_inside_the_marks_transaction_lands_after_it_and_survives() {
    let a = key(1);
    // The mark is a bootstrapping open's second control-plane write, a finishing one's first.
    for (finishing, skip) in [(false, 1), (true, 0)] {
        let dir = tempfile::tempdir().unwrap();
        if finishing {
            died(dir.path(), &a);
        }
        let (hold, inside, go) = barrier();
        let open = opening(dir.path(), OnReading::new(skip, hold), &a);
        arrives(&inside, &open).await;
        assert!(matches!(state(dir.path()), BootstrapState::Unmarked { .. }));
        let (tell, read_at) = channel();
        let root = dir.path().to_owned();
        let at_reading = OnReading::new(0, move || tell.send(state(&root)).unwrap());
        let recovery = recovering(dir.path(), at_reading);
        // One-sided: while the mark's transaction is open, a reading can only come from a
        // recovery that did not wait for it.
        let early = read_at.recv_timeout(Duration::from_millis(200));
        go.send(()).unwrap();
        assert!(early.is_err(), "{early:?}");
        let recovered = recovery.join().unwrap().expect("after the mark");
        let seen = read_at.recv().unwrap();
        assert!(matches!(seen, BootstrapState::Marked { .. }), "{seen:?}");
        let opened = open.await.unwrap().unwrap();
        assert!(admits(&opened.engine, recovered.expose()).await);
        opened.engine.close().await;
        let opened = self::open(dir.path(), Some(&a)).await;
        assert!(admits(&opened.engine, recovered.expose()).await);
        opened.engine.close().await;
        let all = [
            (id(&a), KeyOrigin::Seed),
            (id(recovered.expose()), KeyOrigin::Recovered),
        ];
        assert_eq!(keys(dir.path()), all);
    }
}
