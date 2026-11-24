//! The three kinds of open (D41): a bootstrapping open that fails tries to undo itself
//! before its rename and undoes nothing after it, and a finishing open completes what an earlier
//! one left (TEN-9).

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex};
use std::task::Poll;

use ironflowx_authz::{KeyParts, mint, parse};
use ironflowx_cell::{Ledger, ManualClock, OpenError, new_env_ref};
use ironflowx_core::{Clock, EnvRef, ErrorKind, KeyOrigin, Scope, Secret, Timestamp};
use ironflowx_operations::{Engine, EngineError, Opened, SeedState};
use ironflowx_proto::{Call, EmitRequest, ListEventsRequest, Operations, WhoamiRequest};
use ironflowx_store::{Bootstrap, BootstrapState, ControlPlane};

fn clock() -> ManualClock {
    ManualClock::new(Timestamp::from_millis(1_800_000_000_000))
}

fn uuid(n: u8) -> String {
    format!("019a9f3e-7c2b-7d41-8a3e-1000000000{n:02x}")
}

/// A key's text, its ID from `n` and its secret from `bits`.
fn key(n: u8, bits: u8) -> String {
    let id = format!("019a9f3e-7c2b-7d41-8a3e-2000000000{n:02x}");
    let (secret, _) = mint(id.parse().unwrap(), [bits; 32]);
    secret.expose().to_owned()
}

fn secret(text: &str) -> Secret {
    Secret::new(text.to_owned())
}

fn parts(text: &str) -> KeyParts {
    parse(&secret(text)).unwrap()
}

async fn open(root: &Path, seed: Option<&str>) -> Result<Opened, EngineError> {
    Engine::open(root, clock(), seed.map(secret)).await
}

async fn refused(root: &Path, seed: Option<&str>) -> EngineError {
    open(root, seed).await.err().expect("refused")
}

/// Whether `key` is accepted, and then as an admin key; if not, it must be an invalid one.
async fn admits(engine: &Engine, key: &str) -> bool {
    let cx = Call::with_key(secret(key));
    match engine.whoami(&cx, WhoamiRequest {}).await {
        Ok(me) => {
            assert!(me.scopes.contains(&Scope::Admin), "{:?}", me.scopes);
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

fn ledger(root: &Path, env: EnvRef) -> PathBuf {
    root.join("ledgers").join(env.id().to_string())
}

fn state(root: &Path) -> BootstrapState {
    ControlPlane::bootstrap_state(&control(root)).unwrap()
}

fn unmarked(root: &Path) -> EnvRef {
    match state(root) {
        BootstrapState::Unmarked { env } => env,
        other => panic!("{other:?}"),
    }
}

/// Every key as its ID, origin, and whether it is revoked, oldest first.
fn keys(root: &Path, env: EnvRef) -> Vec<(String, KeyOrigin, bool)> {
    let cp = ControlPlane::open(&control(root)).unwrap();
    let rows = cp.list_keys(&env, None, 10).unwrap();
    rows.into_iter()
        .rev()
        .map(|r| {
            let revoked = r.record.revoked_at.is_some();
            (r.record.id.to_string(), r.origin, revoked)
        })
        .collect()
}

/// The names in `ledgers/`.
fn ledgers(root: &Path) -> Vec<String> {
    let entries = std::fs::read_dir(root.join("ledgers")).unwrap();
    entries
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect()
}

/// Runs `sql` on the control plane from another connection.
fn sql(root: &Path, sql: &str) {
    let db = rusqlite::Connection::open(control(root)).unwrap();
    db.execute_batch(sql).unwrap();
}

type Act = Box<dyn FnOnce() + Send>;

/// A clock that acts inside an open, once as each ledger's writer ends: first when the staging
/// ledger closes, its log already closed and its rename still to come.
#[derive(Clone)]
struct OnWriterEnd(Arc<Mutex<VecDeque<Act>>>);

impl OnWriterEnd {
    fn new(act: impl FnOnce() + Send + 'static) -> Self {
        Self(Arc::new(Mutex::new(VecDeque::from([Box::new(act) as Act]))))
    }

    /// Then acts so as the next writer ends.
    fn then(self, act: impl FnOnce() + Send + 'static) -> Self {
        self.0.lock().unwrap().push_back(Box::new(act));
        self
    }
}

impl Clock for OnWriterEnd {
    fn now(&self) -> Timestamp {
        clock().now()
    }
}

impl Drop for OnWriterEnd {
    fn drop(&mut self) {
        if std::thread::current().name() == Some("ironflowx-ledger-writer") {
            let act = self.0.lock().unwrap().pop_front();
            if let Some(act) = act {
                act();
            }
        }
    }
}

/// A clock that acts once, at its first reading: inside `ensure_bootstrap`'s transaction, after
/// the open has deleted a leftover staging directory.
#[derive(Clone)]
struct OnFirstReading(Arc<Mutex<Option<Act>>>);

impl Clock for OnFirstReading {
    fn now(&self) -> Timestamp {
        if let Some(act) = self.0.lock().unwrap().take() {
            act();
        }
        clock().now()
    }
}

/// What a bootstrapping open that failed before its rename, and undid itself, must leave: nothing
/// of the bootstrap, so that a next open with another seed, or none, bootstraps.
async fn nothing_remains(root: &Path, seed: &str, case: &str) {
    assert_eq!(state(root), BootstrapState::Unbootstrapped, "{case}");
    let db = rusqlite::Connection::open(control(root)).unwrap();
    for table in ["tenants", "projects", "environments", "keys"] {
        let count = format!("SELECT count(*) FROM {table}");
        let rows: u32 = db.query_row(&count, [], |r| r.get(0)).unwrap();
        assert_eq!(rows, 0, "{case}: {table}");
    }
    drop(db);
    if root.join("ledgers").exists() {
        assert_eq!(ledgers(root), [""; 0], "{case}");
    }
    let opened = open(root, None).await.unwrap();
    assert!(!opened.lost_secret && opened.seed.is_none(), "{case}");
    let minted = opened.minted.expect("a fresh bootstrap key");
    assert!(admits(&opened.engine, minted.expose()).await, "{case}");
    assert!(!admits(&opened.engine, seed).await, "{case}");
    opened.engine.close().await;
}

#[tokio::test]
async fn a_bootstrapping_open_that_fails_before_its_rename_leaves_nothing() {
    let a = key(1, 1);
    for given in [None, Some(&*a)] {
        let seed = || given.map(secret);

        let case = "the bootstrap's transaction fails";
        let dir = tempfile::tempdir().unwrap();
        drop(ControlPlane::open(&control(dir.path())).unwrap());
        let fail = "CREATE TRIGGER fail BEFORE INSERT ON keys \
                    BEGIN SELECT RAISE(ABORT, 'injected'); END;";
        sql(dir.path(), fail);
        let err = refused(dir.path(), given).await;
        assert!(matches!(err, EngineError::Store(_)), "{case}: {err:?}");
        sql(dir.path(), "DROP TRIGGER fail");
        nothing_remains(dir.path(), &a, case).await;

        let case = "the ledger cannot be built";
        let dir = tempfile::tempdir().unwrap();
        let staging = dir.path().join("ledgers/.staging");
        let block = OnFirstReading(Arc::new(Mutex::new(Some(Box::new(move || {
            std::fs::create_dir_all(&staging).unwrap();
            std::fs::write(staging.join("log"), "in the way").unwrap();
        })))));
        let err = Engine::open(dir.path(), block, seed()).await.err();
        let built = matches!(err, Some(EngineError::Ledger(OpenError::Io(_))));
        assert!(built, "{case}: {err:?}");
        nothing_remains(dir.path(), &a, case).await;

        let case = "the rename fails";
        let dir = tempfile::tempdir().unwrap();
        let (staging, away) = (dir.path().join("ledgers/.staging"), dir.path().join("away"));
        let root = dir.path().to_owned();
        let block = OnWriterEnd::new(move || {
            unmarked(&root);
            std::fs::rename(staging, away).unwrap();
        });
        let err = Engine::open(dir.path(), block, seed()).await.err();
        assert!(matches!(err, Some(EngineError::Io(_))), "{case}: {err:?}");
        nothing_remains(dir.path(), &a, case).await;
    }
}

/// An undo that fails leaves a committed bootstrap unmarked: the open returns what failed it, not
/// the undo's failure, and the next open finishes the bootstrap, in the same environment.
#[tokio::test]
async fn a_bootstrapping_open_whose_undo_fails_is_finished_by_the_next() {
    let a = key(1, 1);
    for given in [None, Some(&*a)] {
        let dir = tempfile::tempdir().unwrap();
        let (staging, away) = (dir.path().join("ledgers/.staging"), dir.path().join("away"));
        let root = dir.path().to_owned();
        let block = OnWriterEnd::new(move || {
            let fail = "CREATE TRIGGER fail BEFORE DELETE ON keys \
                        BEGIN SELECT RAISE(ABORT, 'injected'); END;";
            sql(&root, fail);
            std::fs::rename(staging, away).unwrap();
        });
        let err = Engine::open(dir.path(), block, given.map(secret)).await;
        let err = err.err().expect("refused");
        let renaming =
            matches!(&err, EngineError::Io(e) if e.kind() == std::io::ErrorKind::NotFound);
        assert!(renaming, "{given:?}: {err:?}");
        let env = unmarked(dir.path());
        assert_eq!(ledgers(dir.path()), [""; 0], "{given:?}");
        let mut rows = keys(dir.path(), env);
        let (origin, id) = match given {
            Some(seed) => (KeyOrigin::Seed, parts(seed).id.to_string()),
            None => (KeyOrigin::Minted, rows[0].0.clone()),
        };
        assert_eq!(rows, [(id, origin, false)]);
        sql(dir.path(), "DROP TRIGGER fail");

        let opened = open(dir.path(), None).await.unwrap();
        assert_eq!(state(dir.path()), BootstrapState::Marked { env });
        assert_eq!(opened.lost_secret, given.is_none());
        assert_eq!(opened.minted.is_some(), given.is_none());
        let works = opened.minted.map_or(a.clone(), |m| m.expose().to_owned());
        assert!(admits(&opened.engine, &works).await, "{given:?}");
        opened.engine.close().await;
        if given.is_none() {
            rows[0].2 = true;
            rows.push((parts(&works).id.to_string(), KeyOrigin::Minted, false));
        }
        assert_eq!(keys(dir.path(), env), rows);
    }
}

/// After its rename a bootstrapping open undoes nothing: the bootstrap stays unmarked, its ledger
/// in place, and the next open finishes it, in the same environment.
#[tokio::test]
async fn a_bootstrapping_open_that_fails_after_its_rename_is_finished_by_the_next() {
    let a = key(1, 1);
    for given in [None, Some(&*a)] {
        for mark_fails in [true, false] {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().to_owned();
            let block = OnWriterEnd::new(move || {
                if mark_fails {
                    let fail = "CREATE TRIGGER fail BEFORE UPDATE ON environments \
                                BEGIN SELECT RAISE(ABORT, 'injected'); END;";
                    sql(&root, fail);
                } else {
                    // The ledger opened at its final name cannot lock its log.
                    let lock = root.join("ledgers/.staging/log/LOCK");
                    std::fs::remove_file(&lock).unwrap();
                    std::fs::create_dir(lock).unwrap();
                }
            });
            let err = Engine::open(dir.path(), block, given.map(secret)).await;
            let err = err.err().expect("refused");
            let env = unmarked(dir.path());
            if mark_fails {
                assert!(matches!(err, EngineError::Store(_)), "{err:?}");
                sql(dir.path(), "DROP TRIGGER fail");
            } else {
                let ledger_failed = matches!(err, EngineError::Ledger(OpenError::Io(_)));
                assert!(ledger_failed, "{err:?}");
                let lock = ledger(dir.path(), env).join("log/LOCK");
                std::fs::remove_dir(lock).unwrap();
            }
            assert_eq!(ledgers(dir.path()), [env.id().to_string()]);
            let before = keys(dir.path(), env);
            assert_eq!(before.len(), 1);
            assert!(!before[0].2, "not revoked yet");

            let opened = open(dir.path(), None).await.unwrap();
            assert_eq!(state(dir.path()), BootstrapState::Marked { env });
            assert_eq!(opened.lost_secret, given.is_none());
            assert_eq!(opened.minted.is_some(), given.is_none());
            let works = opened.minted.map_or(a.clone(), |m| m.expose().to_owned());
            assert!(admits(&opened.engine, &works).await);
            opened.engine.close().await;
        }
    }
}

/// How far an earlier open got before it died.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Died {
    BeforeStaging,
    InStaging,
    AfterRename,
}

/// A root whose first open died unmarked: its environment and its bootstrap key's text.
fn died(root: &Path, at: Died, by_seed: bool) -> (EnvRef, String) {
    let env = new_env_ref();
    let text = key(1, 1);
    let bootstrap = Bootstrap {
        env,
        tenant: uuid(1).parse().unwrap(),
        project: uuid(2).parse().unwrap(),
        key: parts(&text),
        by_seed,
    };
    let cp = ControlPlane::open(&control(root)).unwrap();
    cp.ensure_bootstrap(&bootstrap, &clock()).unwrap();
    match at {
        Died::BeforeStaging => {}
        Died::InStaging => {
            std::fs::create_dir_all(root.join("ledgers/.staging/log")).unwrap();
        }
        Died::AfterRename => {
            drop(Ledger::open_blocking(&ledger(root, env), env, clock()).unwrap());
        }
    }
    (env, text)
}

/// A finishing open with no seed, the bootstrap's own and another, after a minted and after a
/// seeded bootstrap that died at each point: what it reports, and which keys work after it.
#[tokio::test]
async fn a_finishing_open_completes_the_bootstrap_it_finds() {
    let (a, b) = (key(1, 1), key(2, 2));
    for at in [Died::BeforeStaging, Died::InStaging, Died::AfterRename] {
        for (by_seed, given) in [
            (false, None),
            (false, Some(&b)),
            (true, None),
            (true, Some(&a)),
            (true, Some(&b)),
        ] {
            let case = format!("{at:?}, by seed: {by_seed}, given: {given:?}");
            let dir = tempfile::tempdir().unwrap();
            let (env, first) = died(dir.path(), at, by_seed);
            assert_eq!(first, a);
            let opened = open(dir.path(), given.map(String::as_str)).await.unwrap();
            assert_eq!(opened.lost_secret, !by_seed, "{case}");
            assert_eq!(opened.seed, given.map(|_| SeedState::Current), "{case}");
            let replaced = !by_seed && given.is_none();
            assert_eq!(opened.minted.is_some(), replaced, "{case}");
            if let Some(minted) = &opened.minted {
                assert!(admits(&opened.engine, minted.expose()).await, "{case}");
            }
            // The bootstrap's key survives only as a seed that nothing rotated.
            let kept = by_seed && given != Some(&b);
            assert_eq!(admits(&opened.engine, &a).await, kept, "{case}");
            assert_eq!(
                admits(&opened.engine, &b).await,
                given == Some(&b),
                "{case}"
            );
            let admin = opened.minted.as_ref().map(|m| m.expose().to_owned());
            let admin = admin.or(given.cloned()).unwrap_or(a.clone());
            let cx = Call::with_key(secret(&admin));
            opened.engine.emit(&cx, request("a")).await.expect(&case);
            opened.engine.close().await;

            assert_eq!(state(dir.path()), BootstrapState::Marked { env }, "{case}");
            assert_eq!(ledgers(dir.path()), [env.id().to_string()], "{case}");
            let rows = keys(dir.path(), env);
            let admins: Vec<_> = rows.iter().filter(|k| !k.2).map(|k| k.1).collect();
            let origin = if replaced {
                KeyOrigin::Minted
            } else {
                KeyOrigin::Seed
            };
            assert_eq!(admins, [origin], "{case}: one admin key");
            let gone = usize::from(!kept);
            assert_eq!(rows.len(), 1 + gone, "{case}");

            // The bootstrap is finished once: what it lost is not reported again.
            let opened = open(dir.path(), None).await.unwrap();
            assert!(!opened.lost_secret && opened.minted.is_none(), "{case}");
            opened.engine.close().await;
        }
    }
}

fn request(name: &str) -> EmitRequest {
    EmitRequest {
        name: name.into(),
        data: None,
        metadata: None,
        version: None,
        idempotency_key: None,
    }
}

/// A finishing open with a conflicting seed is refused before it marks: no key changes, no secret
/// is lost, and the next open finishes the bootstrap.
#[tokio::test]
async fn a_finishing_open_with_a_conflicting_seed_leaves_the_bootstrap_unmarked() {
    for (by_seed, conflict, case) in [
        (false, key(1, 1), "the minted key's own text"),
        (false, key(1, 2), "the minted key's ID, another secret"),
        (true, key(1, 2), "the seed's ID, another secret"),
    ] {
        for at in [Died::BeforeStaging, Died::InStaging, Died::AfterRename] {
            let case = format!("{at:?}, {case}");
            let dir = tempfile::tempdir().unwrap();
            let (env, _) = died(dir.path(), at, by_seed);
            let before = keys(dir.path(), env);
            let err = refused(dir.path(), Some(&conflict)).await;
            assert!(matches!(err, EngineError::SeedConflict), "{case}: {err:?}");
            assert_eq!(unmarked(dir.path()), env, "{case}");
            assert_eq!(keys(dir.path(), env), before, "{case}");
            let opened = open(dir.path(), None).await.unwrap();
            assert_eq!(opened.lost_secret, !by_seed, "{case}");
            opened.engine.close().await;
        }
    }
}

/// A directory at the ledger's final name that holds neither a log nor `read.db` becomes the
/// ledger, in place: what it held stays (TEN-12).
#[tokio::test]
async fn a_finishing_open_builds_the_ledger_in_a_directory_at_its_final_name() {
    let dir = tempfile::tempdir().unwrap();
    let (env, _) = died(dir.path(), Died::BeforeStaging, true);
    let target = ledger(dir.path(), env);
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(target.join("notes"), "kept").unwrap();
    let opened = open(dir.path(), None).await.unwrap();
    opened.engine.close().await;
    assert_eq!(state(dir.path()), BootstrapState::Marked { env });
    assert!(target.join("log").is_dir());
    assert_eq!(
        std::fs::read_to_string(target.join("notes")).unwrap(),
        "kept"
    );
}

/// A finishing open undoes nothing, whatever fails: an unmarked bootstrap stays one, with its
/// keys and its ledger as they were, and a mark that committed stays too.
#[tokio::test]
async fn a_finishing_open_that_fails_undoes_nothing() {
    let b = key(2, 2);
    let dir = tempfile::tempdir().unwrap();
    let (env, a) = died(dir.path(), Died::AfterRename, true);
    let before = keys(dir.path(), env);
    let log = ledger(dir.path(), env).join("log");
    let kept = |case: &str| {
        assert_eq!(unmarked(dir.path()), env, "{case}");
        assert_eq!(keys(dir.path(), env), before, "{case}");
        assert!(log.exists(), "{case}");
    };

    let holder = Ledger::open_blocking(&ledger(dir.path(), env), env, clock()).unwrap();
    let err = refused(dir.path(), Some(&b)).await;
    assert!(
        matches!(err, EngineError::Ledger(OpenError::Locked)),
        "{err:?}"
    );
    drop(holder);
    kept("the ledger does not open");

    let fail = "CREATE TRIGGER fail BEFORE UPDATE ON environments \
                BEGIN SELECT RAISE(ABORT, 'injected'); END;";
    sql(dir.path(), fail);
    let err = refused(dir.path(), Some(&b)).await;
    assert!(matches!(err, EngineError::Store(_)), "{err:?}");
    sql(dir.path(), "DROP TRIGGER fail");
    kept("the mark fails");

    let fail = "CREATE TRIGGER fail BEFORE INSERT ON keys \
                BEGIN SELECT RAISE(ABORT, 'injected'); END;";
    sql(dir.path(), fail);
    let err = refused(dir.path(), Some(&b)).await;
    assert!(matches!(err, EngineError::Store(_)), "{err:?}");
    sql(dir.path(), "DROP TRIGGER fail");
    assert_eq!(state(dir.path()), BootstrapState::Marked { env });
    assert_eq!(keys(dir.path(), env), before, "the seed was not applied");
    let opened = open(dir.path(), None).await.unwrap();
    assert!(admits(&opened.engine, &a).await);
    assert!(!admits(&opened.engine, &b).await);
    opened.engine.close().await;
}

/// The replacement is stored in the mark's transaction: if it cannot be, the key it would replace
/// is not revoked and the bootstrap stays unmarked.
#[tokio::test]
async fn a_replacement_that_cannot_be_stored_leaves_the_bootstrap_unmarked() {
    let dir = tempfile::tempdir().unwrap();
    let (env, _) = died(dir.path(), Died::AfterRename, false);
    let before = keys(dir.path(), env);
    let fail = "CREATE TRIGGER fail BEFORE INSERT ON keys \
                BEGIN SELECT RAISE(ABORT, 'injected'); END;";
    sql(dir.path(), fail);
    let err = refused(dir.path(), None).await;
    assert!(matches!(err, EngineError::Store(_)), "{err:?}");
    sql(dir.path(), "DROP TRIGGER fail");
    assert_eq!(unmarked(dir.path()), env);
    assert_eq!(keys(dir.path(), env), before);
    let opened = open(dir.path(), None).await.unwrap();
    assert!(opened.lost_secret && opened.minted.is_some());
    opened.engine.close().await;
}

/// An unmarked bootstrap with a ledger at its final name opens that ledger and keeps its records;
/// a foreign one there is refused and kept, never replaced (TEN-12).
#[tokio::test]
async fn a_ledger_at_its_final_name_is_opened_or_refused_and_never_deleted() {
    let dir = tempfile::tempdir().unwrap();
    let a = key(1, 1);
    let cx = Call::with_key(secret(&a));
    let opened = open(dir.path(), Some(&a)).await.unwrap();
    opened.engine.emit(&cx, request("kept")).await.unwrap();
    opened.engine.close().await;
    sql(
        dir.path(),
        "UPDATE environments SET ledger_created_at = NULL",
    );
    let env = unmarked(dir.path());
    let (at, away) = (ledger(dir.path(), env), dir.path().join("away"));

    std::fs::rename(&at, &away).unwrap();
    let foreign = new_env_ref();
    drop(Ledger::open_blocking(&at, foreign, clock()).unwrap());
    let err = refused(dir.path(), None).await;
    let wrong = matches!(err, EngineError::Ledger(OpenError::WrongIncarnation));
    assert!(wrong, "{err:?}");
    assert_eq!(unmarked(dir.path()), env);
    drop(Ledger::open_existing_blocking(&at, foreign, clock()).expect("still there"));

    std::fs::remove_dir_all(&at).unwrap();
    std::fs::rename(&away, &at).unwrap();
    let opened = open(dir.path(), None).await.unwrap();
    let req = ListEventsRequest {
        limit: None,
        cursor: None,
    };
    let page = opened.engine.list_events(&cx, req).await.unwrap();
    let names: Vec<_> = page.events.iter().map(|e| &*e.name).collect();
    assert_eq!(names, ["kept"]);
    opened.engine.close().await;
}

/// Spawns an open of `root` on `clock`, waits until it is `inside`, then cancels it there.
async fn cancel_inside(
    root: &Path,
    clock: impl Clock + Clone + Send + Sync + 'static,
    inside: impl Fn() -> bool,
) {
    let dir = root.to_owned();
    let task = tokio::spawn(async move { Engine::open(&dir, clock, None).await.map(drop) });
    while !inside() {
        assert!(!task.is_finished(), "the open ended before it got there");
        tokio::task::yield_now().await;
    }
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
}

/// How the first open that the root's lock lets in ends.
async fn opened_once_free(root: &Path) -> Result<Opened, EngineError> {
    loop {
        match open(root, None).await {
            Err(EngineError::Locked) => tokio::task::yield_now().await,
            opened => break opened,
        }
    }
}

/// A first open cancelled while its staging ledger closes keeps the root until that ledger is
/// built, since the next open would delete it under the writer, and then until the ledger it
/// opened at its final name, which nothing takes, has closed.
#[tokio::test]
async fn a_cancelled_open_holds_the_root_while_it_builds_its_ledger() {
    let dir = tempfile::tempdir().unwrap();
    let (tell, inside) = channel();
    let (go, wait) = channel::<()>();
    let (tell_end, ending) = channel();
    let (go_end, wait_end) = channel::<()>();
    let held = OnWriterEnd::new(move || {
        tell.send(()).unwrap();
        let _ = wait.recv();
    })
    .then(move || {
        tell_end.send(()).unwrap();
        let _ = wait_end.recv();
    });
    cancel_inside(dir.path(), held, || inside.try_recv().is_ok()).await;
    let building = open(dir.path(), None).await.err();
    go.send(()).unwrap();
    ending.recv().unwrap();
    let closing = open(dir.path(), None).await.err();
    go_end.send(()).unwrap();
    for err in [building, closing] {
        assert!(matches!(err, Some(EngineError::Locked)), "{err:?}");
    }
    let opened = opened_once_free(dir.path()).await;
    let opened = opened.expect("the next open finishes the bootstrap");
    assert!(opened.lost_secret);
    let minted = opened.minted.expect("a replacement");
    assert!(admits(&opened.engine, minted.expose()).await);
    opened.engine.close().await;
}

/// An open cancelled while it opens `control.db` keeps the root until that ends: a connection of
/// this test holds SQLite's write lock, which opening takes.
#[tokio::test]
async fn a_cancelled_open_holds_the_root_while_it_opens_its_control_plane() {
    let dir = tempfile::tempdir().unwrap();
    drop(ControlPlane::open(&control(dir.path())).unwrap());
    let db = rusqlite::Connection::open(control(dir.path())).unwrap();
    db.execute_batch("BEGIN IMMEDIATE").unwrap();
    let mut opening = Box::pin(Engine::open(dir.path(), clock(), None));
    // It is pending once for each blocking step: the layout's, then the control plane's.
    let mut steps = 0;
    std::future::poll_fn(|cx| {
        assert!(opening.as_mut().poll(cx).is_pending());
        steps += 1;
        if steps == 2 {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    })
    .await;
    drop(opening);
    let err = open(dir.path(), None).await.err();
    db.execute_batch("COMMIT").unwrap();
    assert!(matches!(err, Some(EngineError::Locked)), "{err:?}");
    let opened = opened_once_free(dir.path()).await.unwrap();
    assert!(opened.minted.is_some());
    opened.engine.close().await;
}
