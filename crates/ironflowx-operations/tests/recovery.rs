//! A later open cancelled inside its ledger's recovery (TEN-9). Alone in its process: it runs a
//! child process, which until it execs shares every lock file open in this one, so a test that
//! closes a ledger and opens it again beside it could find it locked.

use std::path::Path;

use ironflowx_cell::ManualClock;
use ironflowx_core::Timestamp;
use ironflowx_operations::{Engine, EngineError, Opened};
use ironflowx_store::{BootstrapState, ControlPlane};

async fn open(root: &Path) -> Result<Opened, EngineError> {
    let clock = ManualClock::new(Timestamp::from_millis(1_800_000_000_000));
    Engine::open(root, clock, None).await
}

/// The root stays locked until the recovery ends.
#[tokio::test]
async fn a_cancelled_open_holds_the_root_while_it_recovers_its_ledger() {
    let dir = tempfile::tempdir().unwrap();
    open(dir.path()).await.unwrap().engine.close().await;
    let state = ControlPlane::bootstrap_state(&dir.path().join("control.db")).unwrap();
    let BootstrapState::Marked { env } = state else {
        panic!("{state:?}");
    };
    // A FIFO for the log's segment: opening it to read waits until it is opened to write. The
    // ledger's lock file comes first, so once it is back, the open is inside the recovery.
    let at = dir.path().join("ledgers").join(env.id().to_string());
    let segment = at.join("log/00000000000000000000.seg");
    let kept = dir.path().join("kept.seg");
    std::fs::rename(&segment, &kept).unwrap();
    let made = std::process::Command::new("mkfifo").arg(&segment).status();
    assert!(made.unwrap().success());
    std::fs::remove_file(at.join("ledger.lock")).unwrap();
    let root = dir.path().to_owned();
    let opening = tokio::spawn(async move { open(&root).await.map(drop) });
    while !at.join("ledger.lock").exists() {
        assert!(!opening.is_finished(), "the open ended before it got there");
        tokio::task::yield_now().await;
    }
    opening.abort();
    assert!(opening.await.unwrap_err().is_cancelled());
    let recovering = open(dir.path()).await.err();
    // Both ends, so that this does not wait, and kept open until the recovery has ended. Before
    // any assertion: a recovery left waiting would hang the test.
    let mut both = std::fs::File::options();
    let fifo = both.read(true).write(true).open(&segment).unwrap();
    std::fs::rename(&kept, &segment).unwrap();
    let opened = loop {
        match open(dir.path()).await {
            Err(EngineError::Locked) => tokio::task::yield_now().await,
            opened => break opened,
        }
    };
    drop(fifo);
    let locked = matches!(recovering, Some(EngineError::Locked));
    assert!(locked, "{recovering:?}");
    opened.unwrap().engine.close().await;
}
