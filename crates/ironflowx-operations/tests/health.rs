//! What a host asks an open engine without a key: `Engine::health`, each part alone, and
//! `Engine::faulted`, with no call in flight. Each way the ledger fails is tested on the ledger.

use std::path::Path;
use std::pin::pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Waker};
use std::time::Duration;

use ironflowx_cell::ManualClock;
use ironflowx_core::{Clock, ErrorKind, Timestamp};
use ironflowx_operations::{ControlPlaneState, Engine, Health, LedgerState};
use ironflowx_proto::{Call, EmitRequest, ListEnvironmentsRequest, Operations, WhoamiRequest};

const OK: Health = Health {
    control_plane: ControlPlaneState::Ok,
    ledger: LedgerState::Open,
};
const CONTROL_PLANE_FAILED: Health = Health {
    control_plane: ControlPlaneState::Failed,
    ..OK
};
const LEDGER_UNAVAILABLE: Health = Health {
    ledger: LedgerState::Unavailable,
    ..OK
};

async fn open(dir: &Path) -> (Engine, Call) {
    let clock = ManualClock::new(Timestamp::from_millis(1_800_000_000_000));
    let opened = Engine::open(dir, clock, None).await.unwrap();
    (opened.engine, Call::with_key(opened.minted.unwrap()))
}

/// Runs SQL on the engine's `control.db`, from another connection.
fn control(dir: &Path, sql: &str) {
    let db = rusqlite::Connection::open(dir.join("control.db")).unwrap();
    db.execute_batch(sql).unwrap();
}

/// Makes the fold of the next event fail, and so the ledger, after that event is acknowledged.
async fn break_fold(dir: &Path, engine: &Engine, k: &Call) {
    let listed = engine.list_environments(k, ListEnvironmentsRequest {});
    let env = listed.await.unwrap().remove(0).env;
    let ledger = dir.join("ledgers").join(env.id().to_string());
    let db = rusqlite::Connection::open(ledger.join("read.db")).unwrap();
    let sql = "CREATE TRIGGER t BEFORE INSERT ON events BEGIN SELECT RAISE(ABORT, 'x'); END";
    db.execute_batch(sql).unwrap();
}

fn event() -> EmitRequest {
    EmitRequest {
        name: "order.placed".into(),
        data: None,
        metadata: None,
        version: None,
        idempotency_key: None,
    }
}

/// Whether `f` is ready when polled once.
fn ready<F: Future>(f: std::pin::Pin<&mut F>) -> bool {
    f.poll(&mut Context::from_waker(Waker::noop())).is_ready()
}

#[tokio::test]
async fn health_reports_each_part_alone() {
    let dir = tempfile::tempdir().unwrap();
    let (engine, k) = open(dir.path()).await;
    assert_eq!(engine.health().await, OK);

    // It reads no key record: without one, every call fails.
    control(dir.path(), "ALTER TABLE keys RENAME TO keys_gone");
    let refused = engine.whoami(&k, WhoamiRequest {}).await.unwrap_err();
    assert_eq!(refused.kind, ErrorKind::Unavailable);
    assert_eq!(engine.health().await, OK);
    control(dir.path(), "ALTER TABLE keys_gone RENAME TO keys");

    let gone = "ALTER TABLE environments RENAME TO environments_gone";
    let back = "ALTER TABLE environments_gone RENAME TO environments";
    control(dir.path(), gone);
    assert_eq!(engine.health().await, CONTROL_PLANE_FAILED);
    control(dir.path(), back);
    assert_eq!(engine.health().await, OK, "cleared");

    break_fold(dir.path(), &engine, &k).await;
    let faulted = engine.faulted();
    engine.emit(&k, event()).await.expect("acknowledged");
    faulted.await;
    assert_eq!(engine.health().await, LEDGER_UNAVAILABLE);

    control(dir.path(), gone);
    let both = Health {
        control_plane: ControlPlaneState::Failed,
        ..LEDGER_UNAVAILABLE
    };
    assert_eq!(engine.health().await, both);
    engine.close().await;
}

/// Panics while its flag is set.
#[derive(Clone)]
struct Panicky(Arc<AtomicBool>);

impl Clock for Panicky {
    fn now(&self) -> Timestamp {
        assert!(!self.0.load(Ordering::SeqCst), "the clock panics");
        Timestamp::from_millis(1_800_000_000_000)
    }
}

/// A panic under the control plane's lock fails every later call that reaches it, so health says
/// so though the ledger is open.
#[tokio::test]
async fn a_control_plane_a_panic_locked_is_failed_until_reopened() {
    let dir = tempfile::tempdir().unwrap();
    let panics = Arc::new(AtomicBool::new(false));
    let clock = Panicky(panics.clone());
    let opened = Engine::open(dir.path(), clock, None).await.unwrap();
    let (engine, k) = (opened.engine, Call::with_key(opened.minted.unwrap()));
    panics.store(true, Ordering::SeqCst);
    engine.whoami(&k, WhoamiRequest {}).await.unwrap_err();
    panics.store(false, Ordering::SeqCst);
    assert_eq!(engine.health().await, CONTROL_PLANE_FAILED);
    engine.close().await;
}

/// The fold fails after the emit is answered: the fault is signalled with no call in flight, to a
/// task that borrows nothing of the engine, and at once to whoever asks later.
#[tokio::test]
async fn a_ledger_fault_is_signalled_with_no_call_in_flight() {
    let dir = tempfile::tempdir().unwrap();
    let (engine, k) = open(dir.path()).await;
    engine.emit(&k, event()).await.unwrap();
    let mut pending = pin!(engine.faulted());
    assert!(!ready(pending.as_mut()), "open");
    let task = tokio::spawn(engine.faulted());
    break_fold(dir.path(), &engine, &k).await;
    assert!(!ready(pending.as_mut()), "open");
    engine.emit(&k, event()).await.expect("acknowledged");
    let signalled = tokio::time::timeout(Duration::from_secs(10), task).await;
    signalled.expect("signalled").unwrap();
    assert!(ready(pending));
    let later = engine.faulted();
    assert!(ready(pin!(engine.faulted())), "taken after the fault");
    engine.close().await;
    assert!(ready(pin!(later)), "taken after the fault, closed since");
}

/// A host tells a fault from its own close: a control-plane failure, `close` and a drop are none.
#[tokio::test]
async fn neither_a_control_plane_failure_nor_a_close_is_a_fault() {
    for close in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let (engine, k) = open(dir.path()).await;
        let mut faulted = pin!(engine.faulted());
        control(dir.path(), "ALTER TABLE environments RENAME TO gone");
        engine.whoami(&k, WhoamiRequest {}).await.unwrap_err();
        assert_eq!(engine.health().await, CONTROL_PLANE_FAILED);
        assert!(!ready(faulted.as_mut()));
        if close {
            engine.close().await;
        } else {
            drop(engine);
        }
        assert!(!ready(faulted.as_mut()), "close: {close}");
    }
}
