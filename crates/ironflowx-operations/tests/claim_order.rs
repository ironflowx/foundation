//! RUN-16's order when several things happen at once to a waiting claim: a due run first, then a
//! fault or a drain, which end it as `Unavailable`, then the end of the wait, where it is empty.
//! Its own test binary, since the fault hook is process-wide.
//!
//! The barrier: the first of the pair is a commit, acknowledged and held before its fold, so no
//! read of the claim gets past it; the second happens while it is held; then the fold goes on.

#[allow(dead_code, reason = "shared with tests/claims.rs, which uses the rest")]
mod claiming;

use std::sync::Mutex;
use std::time::Duration;

use claiming::{Fixture, Probe, Stop, T, fixture, receive, unavailable};
use ironflowx_cell::{FaultPoint, set_fault_hook};
use ironflowx_proto::{EmitRequest, Operations};

static AT_ACK: Mutex<Option<Stop>> = Mutex::new(None);

/// Holds the writer after it answers a commit, before the fold, once armed.
fn hold_after_ack(point: FaultPoint) {
    if point != FaultPoint::AfterAck {
        return;
    }
    let stop = AT_ACK.lock().unwrap().take();
    if let Some(stop) = stop {
        stop.reach();
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Event {
    Run,
    Fault,
    Drain,
    End,
}
use Event::{Drain, End, Fault, Run};

async fn emit(f: &Fixture) {
    let req = EmitRequest {
        name: "order.placed".into(),
        data: None,
        metadata: None,
        version: None,
        idempotency_key: None,
    };
    f.engine.emit(&f.admin, req).await.unwrap();
}

/// Fails the read store's next event insert: the ledger fails at the fold of an emit.
async fn fail_next_emit(f: &Fixture) {
    let db = rusqlite::Connection::open(f.ledger_dir().await.join("read.db")).unwrap();
    let sql = "CREATE TRIGGER e BEFORE INSERT ON events BEGIN SELECT RAISE(ABORT, 'x'); END";
    db.execute_batch(sql).unwrap();
}

/// A run and the end of the wait: the run is taken. A run and a fault (its own fold fails): a run
/// the ledger cannot read is no run, so `Unavailable`. A fault, or a drain, and the end of the
/// wait: `Unavailable`, not empty. (A run and a drain: `claims.rs`, held at the read that sees the
/// run, since a drain ends a read held at a fold, as the last case here shows.)
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_run_a_fault_a_drain_and_the_end_of_the_wait_at_one_barrier() {
    set_fault_hook(hold_after_ack);
    for pair in [[Run, End], [Run, Fault], [Fault, End], [Drain, End]] {
        let clock = Probe::at(T);
        let f = fixture(clock.clone()).await;
        let readings = clock.tell_others();
        let claim = f.spawn_claim(Some(1));
        // Its admission, and its read: it found nothing and waits.
        receive(&readings, 2);
        let (stop, acked, go) = Stop::new();
        *AT_ACK.lock().unwrap() = Some(stop);
        let mut run = None;
        match pair {
            [Run, Fault] => {
                f.fail_next_start().await;
                f.start().await;
            }
            [Run, _] => run = Some(f.start().await),
            [Fault, _] => {
                fail_next_emit(&f).await;
                emit(&f).await;
            }
            // A commit of its own holds the claim's reads.
            [Drain, _] => emit(&f).await,
            _ => unreachable!("{pair:?}"),
        }
        receive(&acked, 1);
        for event in pair {
            match event {
                End => tokio::time::sleep(Duration::from_millis(1_100)).await,
                Drain => f.engine.begin_drain(),
                // Committed above.
                Run | Fault => {}
            }
        }
        go.send(()).unwrap();
        let (claimed, _) = claim.await.unwrap();
        match pair {
            [Run, End] => {
                let taken = claimed.unwrap().expect("the run");
                assert_eq!(Some(taken.lease.run.to_string()), run, "{pair:?}");
            }
            _ => assert!(unavailable(&claimed), "{pair:?}: {claimed:?}"),
        }
        f.close().await;
    }
    // RUN-15: a drain ends a claim whose read waits on another call's fold, without waiting for
    // that fold: no claim delays a stop. One test, as the hook is one.
    let clock = Probe::at(T);
    let f = fixture(clock.clone()).await;
    let readings = clock.tell_others();
    let claim = f.spawn_claim(None);
    receive(&readings, 2);
    let (stop, acked, go) = Stop::new();
    *AT_ACK.lock().unwrap() = Some(stop);
    emit(&f).await;
    receive(&acked, 1);
    let drained = std::time::Instant::now();
    f.engine.begin_drain();
    let answered = tokio::time::timeout(Duration::from_secs(2), claim).await;
    go.send(()).unwrap();
    let (claimed, at) = answered.expect("no answer while the fold is held").unwrap();
    assert!(unavailable(&claimed), "{claimed:?}");
    assert!(at - drained < Duration::from_secs(1), "{:?}", at - drained);
    f.close().await;
}
