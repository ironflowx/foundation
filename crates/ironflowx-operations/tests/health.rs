//! What a host asks an open engine without a key: `Engine::health`, each part alone, and
//! `Engine::faulted`, which no close resolves. `tests/fault.rs` has a fault it is told of, and the
//! ledger's own tests each way to one.

use std::path::Path;
use std::pin::pin;
use std::task::{Context, Waker};
use std::time::Duration;

use ironflowx_cell::ManualClock;
use ironflowx_core::{ErrorKind, Timestamp};
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

    // The fold of the next event fails, and so the ledger, after that event is acknowledged.
    let listed = engine.list_environments(&k, ListEnvironmentsRequest {});
    let env = listed.await.unwrap().remove(0).env;
    let ledger = dir.path().join("ledgers").join(env.id().to_string());
    let db = rusqlite::Connection::open(ledger.join("read.db")).unwrap();
    let sql = "CREATE TRIGGER t BEFORE INSERT ON events BEGIN SELECT RAISE(ABORT, 'x'); END";
    db.execute_batch(sql).unwrap();
    let faulted = engine.faulted();
    let event = EmitRequest {
        name: "order.placed".into(),
        data: None,
        metadata: None,
        version: None,
        idempotency_key: None,
    };
    engine.emit(&k, event).await.expect("acknowledged");
    let signalled = tokio::time::timeout(Duration::from_secs(10), faulted).await;
    signalled.expect("signalled");
    assert_eq!(engine.health().await, LEDGER_UNAVAILABLE);

    control(dir.path(), gone);
    let both = Health {
        control_plane: ControlPlaneState::Failed,
        ..LEDGER_UNAVAILABLE
    };
    assert_eq!(engine.health().await, both);
    engine.close().await;
}

/// A host tells a fault from its own close: a control-plane failure, `close` and a drop are none.
#[tokio::test]
async fn neither_a_control_plane_failure_nor_a_close_is_a_fault() {
    for close in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let (engine, k) = open(dir.path()).await;
        let mut faulted = pin!(engine.faulted());
        control(dir.path(), "ALTER TABLE environments RENAME TO gone");
        let refused = engine.whoami(&k, WhoamiRequest {}).await.unwrap_err();
        assert_eq!(refused.kind, ErrorKind::Unavailable);
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
