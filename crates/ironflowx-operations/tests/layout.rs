//! What `Engine::open` refuses by a data directory's layout, and that a refusal changes nothing
//! in it but the lock file and a leftover staging directory (TEN-12). ACC-016–018 cover the same
//! rules as a host sees them.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use ironflowx_authz::mint;
use ironflowx_cell::{Ledger, ManualClock, OpenError, new_env_ref};
use ironflowx_core::{EnvRef, Secret, Timestamp};
use ironflowx_operations::{Engine, EngineError, Opened, SeedState};
use ironflowx_proto::{Call, EmitRequest, ListEventsRequest, Operations};
use ironflowx_store::{BootstrapState, ControlPlane};

fn clock() -> ManualClock {
    ManualClock::new(Timestamp::from_millis(1_800_000_000_000))
}

/// A seed's text: a well-formed key, its ID and its secret from `n`.
fn seed(n: u8) -> Secret {
    let id = format!("019a9f3e-7c2b-7d41-8a3e-2000000000{n:02x}");
    mint(id.parse().unwrap(), [n; 32]).0
}

async fn open(root: &Path, n: u8) -> Result<Opened, EngineError> {
    Engine::open(root, clock(), Some(seed(n))).await
}

/// Fails naming only the paths that differ: whole files make no readable message.
fn unchanged(root: &Path, before: &Contents, case: &str) {
    let after = contents(root);
    let differs = |path: &&PathBuf| before.get(*path) != after.get(*path);
    let paths: Vec<_> = before.keys().chain(after.keys()).filter(differs).collect();
    assert!(paths.is_empty(), "{case}: {paths:?}");
}

type Contents = BTreeMap<PathBuf, Option<Vec<u8>>>;

/// Every file under `root` with its bytes, and every directory, by path; but for the lock file
/// and the staging directory, which a refusal may change.
fn contents(root: &Path) -> Contents {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.ends_with("engine.lock") || path.ends_with(".staging") {
                continue;
            }
            let bytes = path.is_file().then(|| std::fs::read(&path).unwrap());
            if path.is_dir() {
                stack.push(path.clone());
            }
            out.insert(path, bytes);
        }
    }
    out
}

fn control(root: &Path) -> PathBuf {
    root.join("control.db")
}

fn marked(root: &Path) -> EnvRef {
    match ControlPlane::bootstrap_state(&control(root)).unwrap() {
        BootstrapState::Marked { env } => env,
        other => panic!("{other:?}"),
    }
}

fn ledger(root: &Path) -> PathBuf {
    root.join("ledgers").join(marked(root).id().to_string())
}

fn request() -> EmitRequest {
    EmitRequest {
        name: "order.placed".into(),
        data: None,
        metadata: None,
        version: None,
        idempotency_key: None,
    }
}

/// A root bootstrapped with seed 1, holding one event, closed.
async fn engine_root(root: &Path) {
    let opened = open(root, 1).await.unwrap();
    let cx = Call::with_key(seed(1));
    opened.engine.emit(&cx, request()).await.unwrap();
    opened.engine.close().await;
}

/// A loss or a swap, and the refusal it must meet.
type Row = (&'static str, fn(&Path), fn(&EngineError) -> bool);

const ORPHANED: fn(&EngineError) -> bool = |e| matches!(e, EngineError::Orphaned);
const LEDGER_MISSING: fn(&EngineError) -> bool = |e| matches!(e, EngineError::LedgerMissing);

const ROWS: [Row; 14] = [
    (
        "a control plane of format 1, as 0.2 and 0.3 left it (D47)",
        |root| set_format(root, "1"),
        |e| matches!(e, EngineError::Format),
    ),
    (
        "a control plane of a later format",
        |root| set_format(root, "3"),
        |e| matches!(e, EngineError::Format),
    ),
    (
        "a control plane of a format past any this build can name",
        |root| set_format(root, "4294967296"),
        |e| matches!(e, EngineError::Format),
    ),
    (
        "a SQLite file that is no control plane",
        |root| {
            std::fs::remove_file(control(root)).unwrap();
            let db = rusqlite::Connection::open(control(root)).unwrap();
            db.execute_batch("CREATE TABLE notes (text TEXT)").unwrap();
        },
        |e| matches!(e, EngineError::Store(_)),
    ),
    (
        "the control plane deleted",
        |root| std::fs::remove_file(control(root)).unwrap(),
        ORPHANED,
    ),
    (
        "the control plane and the lock file deleted",
        |root| {
            std::fs::remove_file(control(root)).unwrap();
            std::fs::remove_file(root.join("engine.lock")).unwrap();
        },
        ORPHANED,
    ),
    (
        "the control plane replaced by one never bootstrapped",
        |root| {
            std::fs::remove_file(control(root)).unwrap();
            drop(ControlPlane::open(&control(root)).unwrap());
        },
        ORPHANED,
    ),
    (
        "the control plane truncated to nothing",
        |root| std::fs::write(control(root), "").unwrap(),
        ORPHANED,
    ),
    (
        "the control plane deleted, the ledger a link to another directory",
        |root| {
            let away = root.join("volume");
            std::fs::rename(ledger(root), &away).unwrap();
            std::os::unix::fs::symlink(away, ledger(root)).unwrap();
            std::fs::remove_file(control(root)).unwrap();
        },
        ORPHANED,
    ),
    (
        "the ledger deleted",
        |root| std::fs::remove_dir_all(ledger(root)).unwrap(),
        LEDGER_MISSING,
    ),
    (
        "every ledger deleted",
        |root| std::fs::remove_dir_all(root.join("ledgers")).unwrap(),
        LEDGER_MISSING,
    ),
    (
        "the ledger emptied",
        |root| {
            std::fs::remove_dir_all(ledger(root)).unwrap();
            std::fs::create_dir(ledger(root)).unwrap();
        },
        LEDGER_MISSING,
    ),
    (
        "the ledger's log emptied: no genesis record, its read store kept",
        |root| {
            std::fs::remove_dir_all(ledger(root).join("log")).unwrap();
            std::fs::create_dir(ledger(root).join("log")).unwrap();
        },
        LEDGER_MISSING,
    ),
    (
        "the ledger of another incarnation",
        |root| {
            let dir = ledger(root);
            std::fs::remove_dir_all(&dir).unwrap();
            drop(Ledger::open_blocking(&dir, new_env_ref(), clock()).unwrap());
        },
        |e| matches!(e, EngineError::Ledger(OpenError::WrongIncarnation)),
    ),
];

fn set_format(root: &Path, format: &str) {
    let db = rusqlite::Connection::open(control(root)).unwrap();
    db.execute_batch(&format!("UPDATE meta SET format = {format}"))
        .unwrap();
}

/// Each row of the layout table that refuses: the refusal, and every byte of the directory as it
/// was, with a leftover staging directory and without.
#[tokio::test]
async fn a_refused_layout_is_left_as_it_was() {
    for (case, change, refusal) in ROWS {
        for leftover in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            engine_root(dir.path()).await;
            change(dir.path());
            let staging = dir.path().join("ledgers").join(".staging");
            if leftover {
                std::fs::create_dir_all(staging.join("log")).unwrap();
            }
            let before = contents(dir.path());
            // Twice: the first refusal leaves nothing that changes the second's mind.
            for _ in 0..2 {
                let err = open(dir.path(), 2).await.err().expect("refused");
                assert!(refusal(&err), "{case}: {err:?}");
                unchanged(dir.path(), &before, case);
                // Staging goes once the control plane was read.
                let unread = matches!(err, EngineError::Format | EngineError::Store(_));
                assert_eq!(staging.exists(), leftover && unread, "{case}");
            }
        }
    }
}

/// A copy of a root whose engine, bootstrapped with seed 1, is still open, as a crash leaves it:
/// its last rows only in the WAL.
async fn crashed_root(root: &Path) {
    let live = tempfile::tempdir().unwrap();
    let opened = open(live.path(), 1).await.unwrap();
    for (path, bytes) in contents(live.path()) {
        let copy = root.join(path.strip_prefix(live.path()).unwrap());
        match bytes {
            None => std::fs::create_dir_all(copy).unwrap(),
            Some(bytes) => {
                std::fs::create_dir_all(copy.parent().unwrap()).unwrap();
                std::fs::write(copy, bytes).unwrap();
            }
        }
    }
    opened.engine.close().await;
    assert!(root.join("control.db-wal").metadata().unwrap().len() > 0);
}

/// After a crash, a refusal leaves `control.db` and its WAL as they were, and the rows are still
/// there for the next open: a lost ledger, and a seed conflict on a later open and a finishing one.
#[tokio::test]
async fn a_refusal_after_a_crash_keeps_the_control_plane_and_its_wal() {
    for (case, unmark) in [("lost", false), ("later", false), ("finishing", true)] {
        let dir = tempfile::tempdir().unwrap();
        crashed_root(dir.path()).await;
        let files = ["control.db", "control.db-wal"].map(|f| dir.path().join(f));
        let read = || files.clone().map(|f| std::fs::read(f).unwrap());
        if unmark {
            // As a crash would: the row goes to the WAL, and nothing checkpoints it.
            let db = rusqlite::Connection::open(control(dir.path())).unwrap();
            db.execute_batch("UPDATE environments SET ledger_created_at = NULL")
                .unwrap();
            let kept = read();
            drop(db);
            for (file, bytes) in files.iter().zip(kept) {
                std::fs::write(file, bytes).unwrap();
            }
        }
        let before = read();
        let state = ControlPlane::bootstrap_state(&control(dir.path())).unwrap();
        let (BootstrapState::Marked { env } | BootstrapState::Unmarked { env }) = state else {
            panic!("{state:?}");
        };
        assert_eq!(matches!(state, BootstrapState::Unmarked { .. }), unmark);
        let ledger = dir.path().join("ledgers").join(env.id().to_string());
        let away = dir.path().join("away");
        if case == "lost" {
            std::fs::rename(&ledger, &away).unwrap();
            let err = open(dir.path(), 2).await.err().expect("refused");
            assert!(matches!(err, EngineError::LedgerMissing), "{err:?}");
        } else {
            let err = open_conflicting(dir.path()).await;
            assert!(matches!(err, EngineError::SeedConflict), "{err:?}");
        }
        assert!(read() == before, "{case}: the control plane changed");
        if case == "lost" {
            std::fs::rename(&away, &ledger).unwrap();
        }
        let opened = open(dir.path(), 1).await.unwrap();
        assert_eq!(opened.seed, Some(SeedState::Current), "{case}");
        opened.engine.close().await;
    }
}

/// Opens with seed 1's key ID and another secret.
async fn open_conflicting(root: &Path) -> EngineError {
    let id = "019a9f3e-7c2b-7d41-8a3e-200000000001".parse().unwrap();
    let other_secret = mint(id, [9; 32]).0;
    let opened = Engine::open(root, clock(), Some(other_secret)).await;
    opened.err().expect("refused")
}

/// A root whose control plane is gone bootstraps only if `ledgers/` holds nothing that could be a
/// ledger: a leftover staging directory goes, and a plain file is none.
#[tokio::test]
async fn a_root_without_a_ledger_bootstraps() {
    for stray in [".staging/log/LOCK", ".DS_Store"] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledgers").join(stray);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "").unwrap();
        let opened = open(dir.path(), 1).await.unwrap();
        assert!(!dir.path().join("ledgers/.staging").exists());
        opened.engine.close().await;
    }
}

/// A ledger's own directory is refused before the lock file is made in it; a plain file named
/// `log` makes none.
#[tokio::test]
async fn a_bare_ledger_is_refused_with_nothing_created() {
    let dir = tempfile::tempdir().unwrap();
    let env = new_env_ref();
    drop(Ledger::open_blocking(dir.path(), env, clock()).unwrap());
    let before = contents(dir.path());
    let err = open(dir.path(), 1).await.err().expect("refused");
    assert!(matches!(err, EngineError::NotAnEngineRoot), "{err:?}");
    unchanged(dir.path(), &before, "a bare ledger");
    assert!(!dir.path().join("engine.lock").exists());

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("log"), "").unwrap();
    open(dir.path(), 1).await.unwrap().engine.close().await;
}

/// A ledger opens in any directory with neither a log nor `read.db`, an engine's root included;
/// the engine then refuses the root while the ledger's log is in it.
#[tokio::test]
async fn a_ledger_opened_on_an_engines_root_shuts_the_engine_out() {
    let dir = tempfile::tempdir().unwrap();
    engine_root(dir.path()).await;
    drop(Ledger::open_blocking(dir.path(), new_env_ref(), clock()).unwrap());
    let err = open(dir.path(), 1).await.err().expect("refused");
    assert!(matches!(err, EngineError::NotAnEngineRoot), "{err:?}");
    std::fs::remove_dir_all(dir.path().join("log")).unwrap();
    open(dir.path(), 1).await.unwrap().engine.close().await;
}

/// A losing opener changes nothing, staging included: the winner may be building a ledger in it.
#[tokio::test]
async fn a_second_engine_is_locked_out_and_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let opened = open(dir.path(), 1).await.unwrap();
    std::fs::create_dir_all(dir.path().join("ledgers/.staging/log")).unwrap();
    let before = contents(dir.path());
    let err = open(dir.path(), 2).await.err().expect("refused");
    assert!(matches!(err, EngineError::Locked), "{err:?}");
    unchanged(dir.path(), &before, "a held root");
    assert!(dir.path().join("ledgers/.staging/log").exists());
    let cx = Call::with_key(seed(1));
    opened.engine.emit(&cx, request()).await.unwrap();
    opened.engine.close().await;
}

/// A later open refused as a seed conflict writes nothing to the control plane; only the ledger's
/// derived read store may be rebuilt.
#[tokio::test]
async fn a_seed_conflict_on_a_later_open_writes_nothing_to_the_control_plane() {
    let dir = tempfile::tempdir().unwrap();
    engine_root(dir.path()).await;
    std::fs::remove_file(ledger(dir.path()).join("read.db")).unwrap();
    let before = std::fs::read(control(dir.path())).unwrap();
    let err = open_conflicting(dir.path()).await;
    assert!(matches!(err, EngineError::SeedConflict), "{err:?}");
    assert_eq!(std::fs::read(control(dir.path())).unwrap(), before);
    for suffix in ["-wal", "-shm"] {
        assert!(!dir.path().join(format!("control.db{suffix}")).exists());
    }
    let opened = open(dir.path(), 1).await.unwrap();
    let cx = Call::with_key(seed(1));
    let req = ListEventsRequest {
        limit: None,
        cursor: None,
    };
    let page = opened.engine.list_events(&cx, req).await.unwrap();
    assert_eq!(page.events.len(), 1, "the ledger is the one it was");
    opened.engine.close().await;
}
