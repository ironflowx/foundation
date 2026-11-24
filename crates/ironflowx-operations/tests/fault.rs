//! A ledger fault as a host learns of it, with no event call in flight, while `Engine::health` is
//! held in its control-plane read. Its own test binary, since the fault hook is process-wide.

use std::pin::pin;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::task::{Context, Waker};
use std::time::Duration;

use ironflowx_cell::{FaultPoint, set_fault_hook};
use ironflowx_core::{Clock, Timestamp};
use ironflowx_operations::{ControlPlaneState, Engine, Health, LedgerState};
use ironflowx_proto::{Call, EmitRequest, Operations, WhoamiRequest};

/// Where a thread stops: it says it is there, then waits to be let go.
struct Stop(Sender<()>, Receiver<()>);

impl Stop {
    fn new() -> (Self, Receiver<()>, Sender<()>) {
        let ((tell, reached), (go, gone)) = (channel(), channel());
        (Self(tell, gone), reached, go)
    }

    fn reach(self) {
        self.0.send(()).unwrap();
        self.1.recv().unwrap();
    }
}

static WRITER: Mutex<Option<Stop>> = Mutex::new(None);

/// Stops the writer once it has answered, then kills it.
fn stop_writer(point: FaultPoint) {
    if point != FaultPoint::AfterAck {
        return;
    }
    let stop = WRITER.lock().unwrap().take();
    if let Some(stop) = stop {
        stop.reach();
        panic!("the writer is lost");
    }
}

/// Stops its next reading, if it holds a stop.
#[derive(Clone, Default)]
struct Stopping(Arc<Mutex<Option<Stop>>>);

impl Clock for Stopping {
    fn now(&self) -> Timestamp {
        let stop = self.0.lock().unwrap().take();
        if let Some(stop) = stop {
            stop.reach();
        }
        Timestamp::from_millis(1_800_000_000_000)
    }
}

/// Whether `f` is ready when polled once.
fn ready<F: Future>(f: std::pin::Pin<&mut F>) -> bool {
    f.poll(&mut Context::from_waker(Waker::noop())).is_ready()
}

#[tokio::test]
async fn a_fault_is_signalled_with_no_call_in_flight_and_shows_in_a_health_under_way() {
    set_fault_hook(stop_writer);
    let dir = tempfile::tempdir().unwrap();
    let clock = Stopping::default();
    let opened = Engine::open(dir.path(), clock.clone(), None).await.unwrap();
    let (engine, k) = (opened.engine, Call::with_key(opened.minted.unwrap()));
    let mut pending = pin!(engine.faulted());
    // It borrows nothing of the engine.
    let task = tokio::spawn(engine.faulted());

    let (stop, writer_stopped, kill_writer) = Stop::new();
    *WRITER.lock().unwrap() = Some(stop);
    let event = EmitRequest {
        name: "order.placed".into(),
        data: None,
        metadata: None,
        version: None,
        idempotency_key: None,
    };
    engine.emit(&k, event).await.expect("acknowledged");
    writer_stopped.recv().unwrap();

    // A lookup reads the clock under the control plane's lock, so health waits for it.
    let (stop, lookup_stopped, end_lookup) = Stop::new();
    *clock.0.lock().unwrap() = Some(stop);
    let mut lookup = Box::pin(engine.whoami(&k, WhoamiRequest {}));
    assert!(!ready(lookup.as_mut()));
    lookup_stopped.recv().unwrap();
    let mut health = Box::pin(engine.health());
    assert!(!ready(health.as_mut()));
    assert!(!ready(pending.as_mut()), "open");

    kill_writer.send(()).unwrap();
    let signalled = tokio::time::timeout(Duration::from_secs(10), task).await;
    signalled.expect("signalled").unwrap();
    assert!(ready(pending));
    assert!(!ready(health.as_mut()), "held");
    end_lookup.send(()).unwrap();
    lookup.await.expect("whoami");
    let unavailable = Health {
        control_plane: ControlPlaneState::Ok,
        ledger: LedgerState::Unavailable,
    };
    assert_eq!(health.await, unavailable);

    let later = engine.faulted();
    assert!(ready(pin!(engine.faulted())), "taken after the fault");
    engine.close().await;
    assert!(ready(pin!(later)), "taken after the fault, closed since");
}
