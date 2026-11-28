//! RUN-15: no claim's read waits for a fold, so a fold that does not come holds up neither the end
//! of a wait nor a drain. Its own test binary, since the fault hook is process-wide.
//!
//! The barrier: an emit, acknowledged and held before its fold.

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

#[allow(dead_code, reason = "shared with tests/claims.rs, which uses the rest")]
mod claiming;

use claiming::{Probe, Stop, T, fixture, receive, unavailable};
use ironflowx_cell::{FaultPoint, set_fault_hook};
use ironflowx_proto::{EmitRequest, Operations};

static AT_ACK: Mutex<Option<Stop>> = Mutex::new(None);
/// How many reads have waited for a fold.
static WAITED: AtomicUsize = AtomicUsize::new(0);

/// Holds the writer after it answers a commit, before the fold, once armed; counts reads that wait
/// for a fold.
fn hook(point: FaultPoint) {
    match point {
        FaultPoint::AfterAck => {
            let stop = AT_ACK.lock().unwrap().take();
            if let Some(stop) = stop {
                stop.reach();
            }
        }
        FaultPoint::ReadWaitingForFold => {
            WAITED.fetch_add(1, Ordering::SeqCst);
        }
        _ => {}
    }
}

/// A claim that waits 1 s ends empty at its end, and a drained one at once, while another call's
/// fold is held; neither read waited for that fold.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_held_fold_holds_up_neither_the_end_of_a_wait_nor_a_drain() {
    set_fault_hook(hook);
    for drain in [false, true] {
        let clock = Probe::at(T);
        let f = fixture(clock.clone()).await;
        let readings = clock.tell_others();
        let sent = Instant::now();
        let claim = f.spawn_claim(Some(1));
        // Its admission, and its read: it found nothing and waits.
        receive(&readings, 2);
        let (stop, acked, go) = Stop::new();
        *AT_ACK.lock().unwrap() = Some(stop);
        let req = EmitRequest {
            name: "order.placed".into(),
            data: None,
            metadata: None,
            version: None,
            idempotency_key: None,
        };
        f.engine.emit(&f.admin, req).await.unwrap();
        receive(&acked, 1);
        WAITED.store(0, Ordering::SeqCst);
        let at = Instant::now();
        if drain {
            f.engine.begin_drain();
        }
        let answered = tokio::time::timeout(Duration::from_secs(2), claim).await;
        let waited = WAITED.load(Ordering::SeqCst);
        go.send(()).unwrap();
        let (claimed, when) = answered.expect("no answer while the fold is held").unwrap();
        if drain {
            assert!(unavailable(&claimed), "{claimed:?}");
            assert!(when - at < Duration::from_secs(1), "{:?}", when - at);
        } else {
            assert_eq!(claimed, Ok(None));
            // It waited its whole 1 s, counted from before it was sent.
            assert!(when - sent >= Duration::from_secs(1), "{:?}", when - sent);
        }
        assert_eq!(waited, 0, "drain: {drain}: a read waited for the fold");
        f.close().await;
    }
}
