//! The waiting claim (D113): an idle claim appends nothing and never reaches the writer, a key
//! revoked or expired mid-wait takes no run, and a drain or a ledger fault ends a wait after one
//! read more (RUN-15); a waiting claim takes a run as soon as one is committed or comes due, and
//! within 5 s of the clock being set past a due time (RUN-16); a claimable run comes before a
//! fault, a drain and the end of the wait.
//!
//! The barriers are readings of the engine's clock: the writer reads it once per decision, on its
//! own thread; off it, admission reads it once (`lookup_key`), and so does a claim's read, after
//! its snapshot. A test arms the clock after its setup, so it counts only the readings it means.

mod claiming;

use std::time::{Duration, Instant};

use claiming::{Claimed, Fixture, Probe, T, claim, fixture, log, receive, unavailable};
use ironflowx_cell::HostClock;
use ironflowx_core::{Clock, ErrorKind};
use ironflowx_proto::{Operations, RevokeKeyRequest, RunStatus, StartRunRequest};

/// RUN-15: 50 claims wait their whole 20 s for a run of a function nobody started. The log is
/// byte for byte as it was, and the writer never read its clock: no claim was submitted.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fifty_idle_claims_append_nothing_and_never_reach_the_writer() {
    let clock = Probe::at(T);
    let f = fixture(clock.clone()).await;
    let ledger = f.ledger_dir().await;
    let (before, readings) = (log(&ledger), clock.writer_readings());
    let others = clock.tell_others();
    let sent = Instant::now();
    let claims: Vec<_> = (0..50).map(|_| f.spawn_claim(None)).collect();
    for claim in claims {
        let (claimed, at) = claim.await.unwrap();
        assert_eq!(claimed, Ok(None));
        // It waited 20 s, as a claim that names no wait does, and no longer.
        let waited = at - sent;
        assert!(Duration::from_secs(20) <= waited, "{waited:?}");
        assert!(waited < Duration::from_secs(21), "{waited:?}");
    }
    assert_eq!(
        clock.writer_readings(),
        readings,
        "a claim reached the writer"
    );
    // Each claim's admission, and a read at 0, 5, 10, 15 and 20 s: it does not poll.
    assert_eq!(others.try_iter().count(), 50 * 6);
    assert!(log(&ledger) == before, "the log changed");
    f.close().await;
}

/// RUN-15: a key revoked, or expired, while its claim waits takes no run when one is then
/// started: the claim is admitted again before it submits, and refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_key_revoked_or_expired_mid_wait_takes_no_run() {
    for revoked in [true, false] {
        let clock = Probe::at(T);
        let f = fixture(clock.clone()).await;
        let readings = clock.tell_others();
        let claim = f.spawn_claim(None);
        // Its admission, and its read: it found nothing and waits.
        receive(&readings, 2);
        if revoked {
            let req = RevokeKeyRequest {
                key_id: f.executor_id.clone(),
            };
            f.engine.revoke_key(&f.admin, req).await.unwrap();
        } else {
            clock.set(T + 3_600_000);
        }
        let run = f.start().await;
        let (claimed, _) = claim.await.unwrap();
        let e = claimed.unwrap_err();
        assert_eq!(e.kind, ErrorKind::Unauthenticated, "revoked: {revoked}");
        assert_eq!(
            f.status(&run).await,
            RunStatus::Queued,
            "revoked: {revoked}"
        );
        f.close().await;
    }
}

/// RUN-15, D43's boundary: the key is revoked after the claim's last admission, while its
/// submission waits at the writer. The claim takes the run.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_key_revoked_after_the_last_admission_may_take_the_run() {
    let clock = Probe::at(T);
    let f = fixture(clock.clone()).await;
    let run = f.start().await;
    let (at_writer, go) = clock.stop_writer(0);
    let claim = f.spawn_claim(Some(0));
    receive(&at_writer, 1);
    let req = RevokeKeyRequest {
        key_id: f.executor_id.clone(),
    };
    f.engine.revoke_key(&f.admin, req).await.unwrap();
    go.send(()).unwrap();
    let (claimed, _) = claim.await.unwrap();
    let taken = claimed.unwrap().expect("the run");
    assert_eq!(taken.lease.run.to_string(), run);
    assert_eq!(f.status(&run).await, RunStatus::Running);
    f.close().await;
}

/// Sends 50 claims that wait, and returns once each has read and found nothing.
fn fifty_waiting(f: &Fixture, clock: &Probe) -> Vec<tokio::task::JoinHandle<(Claimed, Instant)>> {
    let readings = clock.tell_others();
    let claims = (0..50).map(|_| f.spawn_claim(None)).collect();
    // Each one's admission and read.
    receive(&readings, 100);
    claims
}

/// RUN-15: a drain ends 50 waiting claims as `Unavailable` within 1 s, and a new claim at once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_drain_ends_fifty_waiting_claims_at_once() {
    let clock = Probe::at(T);
    let f = fixture(clock.clone()).await;
    let claims = fifty_waiting(&f, &clock);
    let drained = Instant::now();
    f.engine.begin_drain();
    for claim in claims {
        let (claimed, at) = claim.await.unwrap();
        assert!(unavailable(&claimed), "{claimed:?}");
        assert!(at - drained < Duration::from_secs(1), "{:?}", at - drained);
    }
    // Admitted first (architecture.md, "Each call is admitted first").
    let stranger = ironflowx_proto::Call::with_key(ironflowx_core::Secret::new("nope".into()));
    let refused = f
        .engine
        .claim_run(&stranger, claim(None))
        .await
        .unwrap_err();
    assert_eq!(refused.kind, ErrorKind::Unauthenticated);
    // A new claim, with a run due: it takes nothing. Nothing else drains.
    let run = f.start().await;
    let new = f.engine.claim_run(&f.executor(), claim(None)).await;
    assert!(unavailable(&new), "{new:?}");
    assert_eq!(f.status(&run).await, RunStatus::Queued);
    f.close().await;
}

/// RUN-15: a ledger fault ends 50 waiting claims as `Unavailable` within 1 s.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_ledger_fault_ends_fifty_waiting_claims_at_once() {
    let clock = Probe::at(T);
    let f = fixture(clock.clone()).await;
    let claims = fifty_waiting(&f, &clock);
    f.fail_next_start().await;
    let faulted = f.engine.faulted();
    let req = StartRunRequest {
        function: "charge".into(),
    };
    // Acknowledged, then its fold fails.
    f.engine.start_run(&f.admin, req).await.unwrap();
    faulted.await;
    let at_fault = Instant::now();
    for claim in claims {
        let (claimed, at) = claim.await.unwrap();
        assert!(unavailable(&claimed), "{claimed:?}");
        assert!(
            at - at_fault < Duration::from_secs(1),
            "{:?}",
            at - at_fault
        );
    }
    f.close().await;
}

/// RUN-16: a run committed after the claim's read and before its wait wakes the wait, which
/// subscribed before it read: the claim takes the run at once, not at its next 5 s reading.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_commit_between_the_read_and_the_wait_wakes_the_claim() {
    let clock = Probe::at(T);
    let f = fixture(clock.clone()).await;
    // Past the claim's admission, at its read's reading, after the snapshot.
    let (read, go) = clock.stop_other(1);
    let claim = f.spawn_claim(None);
    receive(&read, 1);
    let run = f.start().await;
    // Folded too, by a read: no change is left to come once the claim goes on.
    assert_eq!(f.status(&run).await, RunStatus::Queued);
    let released = Instant::now();
    go.send(()).unwrap();
    let (claimed, at) = claim.await.unwrap();
    assert_eq!(
        claimed.unwrap().expect("the run").lease.run.to_string(),
        run
    );
    assert!(
        at - released < Duration::from_secs(1),
        "{:?}",
        at - released
    );
    f.close().await;
}

/// RUN-16 on the host's clock: a run due in 2 s is taken when it comes due, with no commit, and
/// not before (RUN-10).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_waiting_claim_takes_a_run_when_it_comes_due() {
    let f = fixture(HostClock).await;
    let run = f.start().await;
    let due = HostClock.now().as_millis() + 2_000;
    f.due_at(&run, due).await;
    let (claimed, _) = f.spawn_claim(None).await.unwrap();
    let taken_at = HostClock.now().as_millis();
    assert_eq!(
        claimed.unwrap().expect("the run").lease.run.to_string(),
        run
    );
    assert!(taken_at >= due, "{} ms early", due - taken_at);
    assert!(taken_at - due < 1_000, "{} ms late", taken_at - due);
    f.close().await;
}

/// RUN-16 on a set clock: the clock is set past a due time a minute away, with no commit; the
/// claim sees it within 5 s.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_clock_set_past_a_due_time_is_seen_within_5_s() {
    let clock = Probe::at(T);
    let f = fixture(clock.clone()).await;
    let run = f.start().await;
    f.due_at(&run, T + 60_000).await;
    let readings = clock.tell_others();
    let claim = f.spawn_claim(None);
    // Its admission, and its read: it is due in a minute.
    receive(&readings, 2);
    clock.set(T + 60_000);
    let set = Instant::now();
    let (claimed, at) = claim.await.unwrap();
    assert_eq!(
        claimed.unwrap().expect("the run").lease.run.to_string(),
        run
    );
    assert!(at - set < Duration::from_millis(5_500), "{:?}", at - set);
    f.close().await;
}

/// Two claims that see one run both submit: one takes it, and the other goes back to waiting and
/// takes the next run started. The first submission is held at the writer until both claims have
/// been admitted to submit.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn of_two_claims_that_see_one_run_the_other_waits_on() {
    let clock = Probe::at(T);
    let f = fixture(clock.clone()).await;
    let readings = clock.tell_others();
    let claims = [f.spawn_claim(None), f.spawn_claim(None)];
    // Each one's admission and read.
    receive(&readings, 4);
    // The start's decision passes; the first claim's stops.
    let (at_writer, go) = clock.stop_writer(1);
    let first = f.start().await;
    // The start's admission, then each claim's read and its admission to submit.
    receive(&readings, 5);
    receive(&at_writer, 1);
    go.send(()).unwrap();
    let [mut a, mut b] = claims;
    let taken = |c: Claimed| c.unwrap().map(|c| c.lease.run.to_string());
    let (winner, loser) = tokio::select! {
        r = &mut a => (taken(r.unwrap().0), b),
        r = &mut b => (taken(r.unwrap().0), a),
    };
    assert_eq!(winner.as_deref(), Some(first.as_str()));
    let second = f.start().await;
    assert_eq!(taken(loser.await.unwrap().0), Some(second));
    f.close().await;
}

/// The event clock never goes back (D7): with the clock set back an hour behind the last record, a
/// run due at that record is due now to the claim's read, as to the writer, and a claim takes it at
/// once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_run_due_at_the_last_record_is_due_with_the_clock_set_back() {
    let clock = Probe::at(T);
    let f = fixture(clock.clone()).await;
    clock.set(T + 3_600_000);
    let run = f.start().await;
    clock.set(T);
    let (claimed, _) = f.spawn_claim(Some(1)).await.unwrap();
    assert_eq!(
        claimed.unwrap().expect("the run").lease.run.to_string(),
        run
    );
    f.close().await;
}

/// RUN-16's order when several happen at once: a claimable run, then a fault or a drain, then the
/// end of the wait. The claim is held at the clock reading after its first read's snapshot, which
/// saw no claimable run, while both happen; then it goes on. A run folded, or come due by the
/// clock, since that read is taken by one read more, whatever came with it. A run on a ledger that
/// then failed cannot be read or taken.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_run_a_fault_a_drain_and_the_end_of_the_wait_at_one_barrier() {
    #[derive(Debug, Clone, Copy, PartialEq)]
    enum Event {
        /// A run started and folded.
        Run,
        /// The clock set past the due time of a run, a minute away at the read.
        Due,
        Fault,
        Drain,
        End,
    }
    use Event::{Drain, Due, End, Fault, Run};
    let pairs = [
        [Run, End],
        [Run, Drain],
        [Due, End],
        [Due, Drain],
        [Run, Fault],
        [Fault, End],
        [Drain, End],
    ];
    for pair in pairs {
        let clock = Probe::at(T);
        let f = fixture(clock.clone()).await;
        let mut run = None;
        if pair[0] == Due {
            let started = f.start().await;
            f.due_at(&started, T + 60_000).await;
            run = Some(started);
        }
        let (read, go) = clock.stop_other(1);
        let claim = f.spawn_claim(Some(1));
        receive(&read, 1);
        for event in pair {
            match event {
                Run => {
                    let started = f.start().await;
                    // Folded, by a read.
                    assert_eq!(f.status(&started).await, RunStatus::Queued);
                    run = Some(started);
                }
                Due => clock.set(T + 60_000),
                Fault => {
                    f.fail_next_start().await;
                    let req = StartRunRequest {
                        function: "charge".into(),
                    };
                    f.engine.start_run(&f.admin, req).await.unwrap();
                    f.engine.faulted().await;
                }
                Drain => f.engine.begin_drain(),
                End => tokio::time::sleep(Duration::from_millis(1_100)).await,
            }
        }
        go.send(()).unwrap();
        let (claimed, _) = claim.await.unwrap();
        if matches!(pair, [Run | Due, End | Drain]) {
            let taken = claimed.unwrap().expect("the run");
            assert_eq!(Some(taken.lease.run.to_string()), run, "{pair:?}");
        } else {
            assert!(unavailable(&claimed), "{pair:?}: {claimed:?}");
        }
        f.close().await;
    }
}

/// A fault or a drain that comes during a read begun after the wait was over ends the claim as
/// `Unavailable`, not empty: one read more meets it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_fault_or_a_drain_during_the_last_read_is_no_empty_claim() {
    for fault in [true, false] {
        let clock = Probe::at(T);
        let f = fixture(clock.clone()).await;
        // A claim that does not wait: its one read begins with its wait over.
        let (read, go) = clock.stop_other(1);
        let claim = f.spawn_claim(Some(0));
        receive(&read, 1);
        if fault {
            f.fail_next_start().await;
            let req = StartRunRequest {
                function: "charge".into(),
            };
            f.engine.start_run(&f.admin, req).await.unwrap();
            f.engine.faulted().await;
        } else {
            f.engine.begin_drain();
        }
        go.send(()).unwrap();
        let (claimed, _) = claim.await.unwrap();
        assert!(unavailable(&claimed), "fault: {fault}: {claimed:?}");
        f.close().await;
    }
}

/// With the clock behind the last record, event time stands still until the clock passes it: a run
/// due 5 ms after that record is an hour away by the clock, and a claim that waits 1 s reads twice,
/// not every 5 ms.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_claim_behind_the_last_record_does_not_spin() {
    let clock = Probe::at(T);
    let f = fixture(clock.clone()).await;
    clock.set(T + 3_600_000);
    let run = f.start().await;
    f.due_at(&run, T + 3_600_005).await;
    clock.set(T);
    let readings = clock.tell_others();
    let (claimed, _) = f.spawn_claim(Some(1)).await.unwrap();
    assert_eq!(claimed, Ok(None));
    // Its admission, its read, and its read at the end of the wait.
    assert_eq!(readings.try_iter().count(), 3);
    f.close().await;
}

/// The read's event time after an open is the later of the clock and the last record, as the
/// writer's is (D7): reopened with the clock an hour behind the record that started a run, a claim
/// takes the run at once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn after_an_open_a_run_due_at_the_last_record_is_due() {
    let clock = Probe::at(T);
    let f = fixture(clock.clone()).await;
    clock.set(T + 3_600_000);
    let run = f.start().await;
    let f = f.reopen(Probe::at(T)).await;
    let (claimed, _) = f.spawn_claim(Some(1)).await.unwrap();
    assert_eq!(
        claimed.unwrap().expect("the run").lease.run.to_string(),
        run
    );
    f.close().await;
}

/// RUN-16's order: a due run comes before a drain. The run is committed and folded, and the drain
/// begins while the claim is held at the clock reading of the read that sees the run.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_due_run_comes_before_a_drain_at_one_barrier() {
    let clock = Probe::at(T);
    let f = fixture(clock.clone()).await;
    let readings = clock.tell_others();
    // The claim's admission and first read, the start's admission, then the claim's next read.
    let (read, go) = clock.stop_other(3);
    let claim = f.spawn_claim(None);
    receive(&readings, 2);
    let run = f.start().await;
    receive(&read, 1);
    f.engine.begin_drain();
    go.send(()).unwrap();
    let (claimed, _) = claim.await.unwrap();
    assert_eq!(
        claimed.unwrap().expect("the run").lease.run.to_string(),
        run
    );
    f.close().await;
}

/// RUN-16's order: a due run comes before a drain that begins after the run is folded, when the
/// claim reads only after both. The claim is driven to its wait, then not polled while the run is
/// started and folded and the drain begins.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_folded_run_comes_before_a_later_drain() {
    let clock = Probe::at(T);
    let f = fixture(clock.clone()).await;
    let readings = clock.tell_others();
    let (taken, run) = {
        let cx = f.executor();
        let mut claim = std::pin::pin!(f.engine.claim_run(&cx, claim(None)));
        // Its admission and its read: it found nothing and waits.
        let mut read = 0;
        while read < 2 {
            tokio::select! {
                biased;
                early = &mut claim => panic!("answered before a run existed: {early:?}"),
                () = tokio::time::sleep(Duration::from_millis(5)) => {}
            }
            read += readings.try_iter().count();
        }
        let run = f.start().await;
        assert_eq!(f.status(&run).await, RunStatus::Queued);
        f.engine.begin_drain();
        (claim.await, run)
    };
    assert_eq!(taken.unwrap().expect("the run").lease.run.to_string(), run);
    f.close().await;
}

/// RUN-16's order: a run that comes due while the claim naps, by the clock set past its due time,
/// comes before a drain that follows: the claim, woken by the drain, reads and takes it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_run_come_due_comes_before_a_drain() {
    let clock = Probe::at(T);
    let f = fixture(clock.clone()).await;
    let run = f.start().await;
    f.due_at(&run, T + 60_000).await;
    let readings = clock.tell_others();
    let claim = f.spawn_claim(None);
    // Its admission, and its read: due in a minute, it naps 5 s.
    receive(&readings, 2);
    clock.set(T + 60_000);
    f.engine.begin_drain();
    let (claimed, _) = claim.await.unwrap();
    assert_eq!(
        claimed.unwrap().expect("the run").lease.run.to_string(),
        run
    );
    f.close().await;
}

/// A claim already submitted to the writer when the drain begins is not ended by it (D72): the
/// writer gives it the run, and it answers with that run.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_claim_at_the_writer_when_the_drain_begins_gets_the_writers_answer() {
    let clock = Probe::at(T);
    let f = fixture(clock.clone()).await;
    let run = f.start().await;
    let (at_writer, go) = clock.stop_writer(0);
    let claim = f.spawn_claim(Some(0));
    receive(&at_writer, 1);
    f.engine.begin_drain();
    go.send(()).unwrap();
    let (claimed, _) = claim.await.unwrap();
    assert_eq!(
        claimed.unwrap().expect("the run").lease.run.to_string(),
        run
    );
    f.close().await;
}

/// A key revoked while its claim waits, then a run made claimable, then a drain: the read after
/// the drain finds the run, and admission refuses the key. The run stays queued. The claim is
/// driven to its wait, then not polled while all three happen.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_revoked_key_then_a_run_then_a_drain_is_unauthenticated() {
    let clock = Probe::at(T);
    let f = fixture(clock.clone()).await;
    let readings = clock.tell_others();
    let (claimed, run) = {
        let cx = f.executor();
        let mut claim = std::pin::pin!(f.engine.claim_run(&cx, claim(None)));
        // Its admission and its read: it found nothing and waits.
        let mut read = 0;
        while read < 2 {
            tokio::select! {
                biased;
                early = &mut claim => panic!("answered before a run existed: {early:?}"),
                () = tokio::time::sleep(Duration::from_millis(5)) => {}
            }
            read += readings.try_iter().count();
        }
        let req = RevokeKeyRequest {
            key_id: f.executor_id.clone(),
        };
        f.engine.revoke_key(&f.admin, req).await.unwrap();
        let run = f.start().await;
        assert_eq!(f.status(&run).await, RunStatus::Queued);
        f.engine.begin_drain();
        (claim.await, run)
    };
    assert_eq!(claimed.unwrap_err().kind, ErrorKind::Unauthenticated);
    assert_eq!(f.status(&run).await, RunStatus::Queued);
    f.close().await;
}

/// Two claims that see one run both submit, and the drain begins while they are at the writer:
/// the winner answers with the run; the loser, a waiting claim again, reads once more, finds no
/// run and answers `Unavailable`. Nothing more is appended.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_claim_that_loses_its_run_at_the_writer_during_a_drain_waits_again() {
    let clock = Probe::at(T);
    let f = fixture(clock.clone()).await;
    let readings = clock.tell_others();
    let claims = [f.spawn_claim(None), f.spawn_claim(None)];
    // Each one's admission and read.
    receive(&readings, 4);
    // The start's decision passes; the first claim's stops.
    let (at_writer, go) = clock.stop_writer(1);
    let written = clock.writer_readings();
    let run = f.start().await;
    // The start's admission, then each claim's read and its admission to submit.
    receive(&readings, 5);
    receive(&at_writer, 1);
    f.engine.begin_drain();
    go.send(()).unwrap();
    let mut answers = Vec::new();
    for claim in claims {
        answers.push(claim.await.unwrap().0);
    }
    let taken: Vec<_> = answers
        .iter()
        .filter(|a| matches!(a, Ok(Some(_))))
        .collect();
    assert_eq!(taken.len(), 1, "{answers:?}");
    let Ok(Some(won)) = taken[0] else {
        unreachable!()
    };
    assert_eq!(won.lease.run.to_string(), run);
    assert_eq!(
        answers.iter().filter(|a| unavailable(a)).count(),
        1,
        "{answers:?}"
    );
    // The start and the two claims' decisions, and no other.
    assert_eq!(clock.writer_readings(), written + 3);
    // The loser read once more before it answered.
    assert_eq!(readings.try_iter().count(), 1);
    f.close().await;
}
