//! Execution operations through the engine: the fence on every call that names a lease, leases
//! lost and released, claims that race, and a run's end with its event.

use std::collections::BTreeSet;
use std::sync::Arc;

use ironflowx_cell::ManualClock;
use ironflowx_core::{ErrorKind, Lease, Scope, Timestamp};
use ironflowx_operations::Engine;
use ironflowx_proto::{
    BeginStepRequest, Call, ClaimRunRequest, CompleteRunRequest, CompleteStepRequest,
    CreateKeyRequest, GetRunRequest, HeartbeatRequest, ListEventsRequest, ListFunctionsRequest,
    OpError, Operations, PutFunctionRequest, ReleaseRunRequest, Run, RunStatus, StartRunRequest,
    WhoamiRequest,
};

const T: i64 = 1_800_000_000_000;

struct Fixture {
    engine: Engine,
    admin: Call,
    executor: Call,
    other: Call,
    clock: ManualClock,
    _dir: tempfile::TempDir,
}

async fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let clock = ManualClock::new(Timestamp::from_millis(T));
    let opened = Engine::open(dir.path(), clock.clone(), None).await.unwrap();
    let engine = opened.engine;
    let admin = Call::with_key(opened.minted.unwrap());
    let mut execute = Vec::new();
    for _ in 0..2 {
        let req = CreateKeyRequest {
            scopes: BTreeSet::from([Scope::Execute]),
            ..CreateKeyRequest::default()
        };
        execute.push(Call::with_key(
            engine.create_key(&admin, req).await.unwrap().key,
        ));
    }
    let other = execute.pop().unwrap();
    let executor = execute.pop().unwrap();
    let put = PutFunctionRequest {
        name: "charge".into(),
        code: "v1".into(),
        ..PutFunctionRequest::default()
    };
    engine.put_function(&admin, put).await.unwrap();
    Fixture {
        engine,
        admin,
        executor,
        other,
        clock,
        _dir: dir,
    }
}

impl Fixture {
    async fn start(&self) -> String {
        let req = StartRunRequest {
            function: "charge".into(),
        };
        let started = self.engine.start_run(&self.admin, req).await.unwrap();
        started.run_id.to_string()
    }

    async fn claim(&self) -> Lease {
        self.claim_as(&self.executor).await.expect("a due run")
    }

    async fn claim_as(&self, cx: &Call) -> Option<Lease> {
        let req = ClaimRunRequest {
            functions: vec!["charge@v1".into()],
            ..ClaimRunRequest::default()
        };
        let claim = self.engine.claim_run(cx, req).await.unwrap();
        claim.map(|c| c.lease)
    }

    async fn run(&self, run_id: &str) -> Run {
        let req = GetRunRequest {
            run_id: run_id.to_owned(),
        };
        self.engine.get_run(&self.admin, req).await.unwrap()
    }

    /// Calls `call` on `lease` as `cx`, keeping only how it failed.
    async fn call(&self, call: &str, cx: &Call, lease: &str) -> Result<(), OpError> {
        let lease = lease.to_owned();
        match call {
            "begin" => {
                let req = BeginStepRequest {
                    lease,
                    position: 1,
                    name: "pay".into(),
                    ..BeginStepRequest::default()
                };
                self.engine.begin_step(cx, req).await.map(drop)
            }
            "complete-step" => {
                let req = CompleteStepRequest {
                    lease,
                    position: 1,
                    output: None,
                };
                self.engine.complete_step(cx, req).await.map(drop)
            }
            "complete" => {
                let req = CompleteRunRequest {
                    lease,
                    steps: 1,
                    output: None,
                };
                self.engine.complete_run(cx, req).await.map(drop)
            }
            // A heartbeat of one lease: its one result.
            "heartbeat" => {
                let req = HeartbeatRequest {
                    leases: vec![lease],
                };
                let results = self.engine.heartbeat(cx, req).await?.results;
                let [result] = results.as_slice() else {
                    panic!("one result: {results:?}");
                };
                result.clone().map(drop)
            }
            "release" => {
                let req = ReleaseRunRequest { lease };
                self.engine.release_run(cx, req).await.map(drop)
            }
            _ => unreachable!(),
        }
    }
}

/// Every call that names a lease.
const CALLS: [&str; 5] = ["begin", "complete-step", "complete", "heartbeat", "release"];

/// RUN-1: every call that names a lease meets the fence, in its order, and a refused one changes
/// nothing a client can read.
#[tokio::test]
async fn the_fence_holds_every_call_that_names_a_lease() {
    let f = fixture().await;
    let run_id = f.start().await;
    let lease = f.claim().await;
    // Step 1 open, so each call below would act if the fence let it.
    f.call("begin", &f.executor, &lease.to_string())
        .await
        .unwrap();
    let unknown = Lease {
        run: "0190b5f0-0000-7000-8000-000000000999".parse().unwrap(),
        execution: 1,
    };
    let stale = Lease {
        execution: 2,
        ..lease
    };
    let superseded = Some("this execution was superseded: stop working on this run");
    let foreign = Some("another key holds this lease");
    let cases = [
        (unknown.to_string(), &f.executor, ErrorKind::NotFound, None),
        (
            "not-a-lease".to_owned(),
            &f.executor,
            ErrorKind::NotFound,
            None,
        ),
        (
            stale.to_string(),
            &f.executor,
            ErrorKind::Conflict,
            superseded,
        ),
        (lease.to_string(), &f.other, ErrorKind::Conflict, foreign),
        // Wrong in both: the first failure answers.
        (stale.to_string(), &f.other, ErrorKind::Conflict, superseded),
    ];
    for call in CALLS {
        for (lease, cx, kind, hint) in &cases {
            let before = f.run(&run_id).await;
            let err = f.call(call, cx, lease).await.unwrap_err();
            assert_eq!(
                (err.kind, err.hint.as_deref()),
                (*kind, *hint),
                "{call} {lease}"
            );
            assert!(!err.kind.retryable());
            assert_eq!(f.run(&run_id).await, before, "{call} {lease}");
        }
    }
    let run = f.run(&run_id).await;
    assert_eq!(
        (run.status, run.step.as_deref()),
        (RunStatus::Running, Some("pay"))
    );
    // Superseded for real: the lease lapsed and another key claimed the run.
    f.clock.step(60_000);
    let second = f.claim_as(&f.other).await.expect("a lapsed lease is taken");
    assert_eq!(second.execution, 2);
    for call in CALLS {
        let before = f.run(&run_id).await;
        let err = f
            .call(call, &f.executor, &lease.to_string())
            .await
            .unwrap_err();
        assert_eq!(
            (err.kind, err.hint.as_deref()),
            (ErrorKind::Conflict, superseded),
            "{call}"
        );
        assert_eq!(f.run(&run_id).await, before, "{call}");
    }
    let run = f.run(&run_id).await;
    assert_eq!((run.execution, run.lost_leases), (2, 1));
    f.engine.close().await;
}

/// The fence's table for a released run: until the next claim its execution's calls are
/// `Conflict`, "this execution is over", changing nothing, and its release answers the same due
/// time; the next claim, at once, counts nothing.
#[tokio::test]
async fn a_released_execution_is_over_until_the_next_claim() {
    let f = fixture().await;
    let run_id = f.start().await;
    let lease = f.claim().await.to_string();
    f.call("begin", &f.executor, &lease).await.unwrap();
    let req = || ReleaseRunRequest {
        lease: lease.clone(),
    };
    f.clock.step(1_000);
    let due_at = f.engine.release_run(&f.executor, req()).await.unwrap();
    assert_eq!(due_at, Timestamp::from_millis(T + 1_000));
    let run = f.run(&run_id).await;
    assert_eq!(
        (run.status, run.due_at, run.lease_until, run.step.as_deref()),
        (RunStatus::Queued, Some(due_at), None, Some("pay"))
    );
    f.clock.step(1_000);
    let over = Some("this execution is over");
    for call in ["begin", "complete-step", "complete", "heartbeat"] {
        let before = f.run(&run_id).await;
        let err = f.call(call, &f.executor, &lease).await.unwrap_err();
        assert_eq!(
            (err.kind, err.hint.as_deref()),
            (ErrorKind::Conflict, over),
            "{call}"
        );
        assert_eq!(f.run(&run_id).await, before, "{call}");
    }
    let before = f.run(&run_id).await;
    assert_eq!(f.engine.release_run(&f.executor, req()).await, Ok(due_at));
    assert_eq!(f.run(&run_id).await, before, "a release sent again");
    let next = f.claim_as(&f.other).await.expect("due at once");
    let run = f.run(&run_id).await;
    assert_eq!((next.execution, run.lost_leases), (2, 0));
    f.engine.close().await;
}

/// RUN-8: 50 claims racing for one run, queued and then with its lease lapsed: one takes it each
/// time, and the execution number goes up by one.
#[tokio::test]
async fn of_50_claims_racing_for_one_run_one_takes_it() {
    let f = fixture().await;
    let run_id = f.start().await;
    let Fixture {
        engine,
        clock,
        admin,
        ..
    } = f;
    let engine = Arc::new(engine);
    for execution in [1, 2] {
        let mut racing = tokio::task::JoinSet::new();
        for i in 0..50 {
            let engine = engine.clone();
            racing.spawn(async move {
                let cx = engine.embedder_call(&format!("racer-{i}")).unwrap();
                let req = ClaimRunRequest {
                    functions: vec!["charge@v1".into()],
                    ..ClaimRunRequest::default()
                };
                engine.claim_run(&cx, req).await.unwrap()
            });
        }
        let claims: Vec<_> = racing.join_all().await.into_iter().flatten().collect();
        let [claim] = claims.as_slice() else {
            panic!("one claim takes the run: {claims:?}");
        };
        assert_eq!(claim.lease.execution, execution);
        assert_eq!(claim.lease.run.to_string(), run_id);
        clock.step(60_000);
    }
    let req = GetRunRequest { run_id };
    let run = engine.get_run(&admin, req).await.unwrap();
    assert_eq!((run.execution, run.lost_leases), (2, 1));
    let engine = Arc::try_unwrap(engine).ok().expect("the racers are done");
    engine.close().await;
}

/// RUN-9 through the engine: one lost lease counts 1, and the tenth in a row fails the run with
/// `ironflowx.executor_lost` and `ironflowx.run.failed`, emitted as the key whose claim failed it.
#[tokio::test]
async fn the_tenth_lost_lease_fails_the_run_with_its_event() {
    let f = fixture().await;
    let run_id = f.start().await;
    f.claim().await;
    for lost in 1..=9 {
        f.clock.step(60_000);
        f.claim_as(&f.other).await.expect("a lapsed lease is taken");
        assert_eq!(f.run(&run_id).await.lost_leases, lost);
    }
    f.clock.step(60_000);
    assert_eq!(f.claim_as(&f.executor).await, None);
    let run = f.run(&run_id).await;
    let error = run.error.clone().expect("an error");
    assert_eq!(
        (
            run.status,
            run.execution,
            run.lost_leases,
            error.code.as_str()
        ),
        (RunStatus::Failed, 10, 10, "ironflowx.executor_lost")
    );
    assert_eq!(
        (run.ended_at, run.lease_until),
        (Some(Timestamp::from_millis(T + 600_000)), None)
    );
    let events = f.engine.list_events(
        &f.admin,
        ListEventsRequest {
            limit: None,
            cursor: None,
        },
    );
    let events = events.await.unwrap().events;
    let [event] = events.as_slice() else {
        panic!("one event: {events:?}");
    };
    let me = f
        .engine
        .whoami(&f.executor, WhoamiRequest {})
        .await
        .unwrap();
    assert_eq!(event.name, "ironflowx.run.failed");
    assert_eq!(
        event.emitted_by.to_string(),
        format!("key:{}", me.key_id.expect("a key's call"))
    );
    let data = format!(
        r#"{{"run_id":"{run_id}","function":"charge","code":"v1","error":{{"code":"ironflowx.executor_lost","message":"{}"}}}}"#,
        error.message
    );
    assert_eq!(String::from_utf8(event.data.clone()).unwrap(), data);
    f.engine.close().await;
}

/// RUN-2, TEN-6: a run's end is `ironflowx.run.completed`, emitted as the key that completed it,
/// naming the run, its function and code label, and no output.
#[tokio::test]
async fn a_completed_run_is_an_event_of_its_executor() {
    let f = fixture().await;
    let run_id = f.start().await;
    let lease = f.claim().await;
    let req = CompleteRunRequest {
        lease: lease.to_string(),
        steps: 0,
        output: Some(br#"{"ok": true}"#.to_vec()),
    };
    let end = f
        .engine
        .complete_run(&f.executor, req.clone())
        .await
        .unwrap();
    assert_eq!(
        (end.run_id.to_string(), end.status),
        (run_id.clone(), RunStatus::Completed)
    );
    // Sent again: the same answer, and no second event.
    let again = f.engine.complete_run(&f.executor, req).await.unwrap();
    assert_eq!(again, end);
    let events = f.engine.list_events(
        &f.admin,
        ListEventsRequest {
            limit: None,
            cursor: None,
        },
    );
    let events = events.await.unwrap().events;
    let [event] = events.as_slice() else {
        panic!("one event: {events:?}");
    };
    let me = f
        .engine
        .whoami(&f.executor, WhoamiRequest {})
        .await
        .unwrap();
    assert_eq!(event.name, "ironflowx.run.completed");
    assert_eq!(
        event.emitted_by.to_string(),
        format!("key:{}", me.key_id.expect("a key's call"))
    );
    let data = format!(r#"{{"run_id":"{run_id}","function":"charge","code":"v1"}}"#);
    assert_eq!(String::from_utf8(event.data.clone()).unwrap(), data);
    assert_eq!(
        (event.version, &event.metadata, &event.idempotency_key),
        (1, &None, &None)
    );
    let run = f.run(&run_id).await;
    assert_eq!(run.status, RunStatus::Completed);
    assert_eq!(run.output.as_deref(), Some(br#"{"ok": true}"#.as_slice()));
    assert_eq!(run.ended_at, Some(Timestamp::from_millis(T)));
    assert_eq!((run.execution, run.lease_until), (1, None));
    f.engine.close().await;
}

/// A start answers its run, queued, due at once, with a `null` input and no duplicate; an unknown
/// function, to start or to read, or run, is `NotFound`.
#[tokio::test]
async fn a_started_run_reads_back_queued() {
    let f = fixture().await;
    let req = StartRunRequest {
        function: "charge".into(),
    };
    let started = f.engine.start_run(&f.admin, req).await.unwrap();
    assert!(!started.duplicate);
    let run = f.run(&started.run_id.to_string()).await;
    let me = f.engine.whoami(&f.admin, WhoamiRequest {}).await.unwrap();
    assert_eq!(run.status, RunStatus::Queued);
    assert_eq!(run.input.as_deref(), Some(b"null".as_slice()));
    assert_eq!(run.due_at, Some(Timestamp::from_millis(T)));
    assert_eq!(
        run.created_by.to_string(),
        format!("key:{}", me.key_id.expect("a key's call"))
    );
    assert_eq!((run.execution, run.steps, run.lease_until), (0, 0, None));
    let req = StartRunRequest {
        function: "refund".into(),
    };
    let err = f.engine.start_run(&f.admin, req).await.unwrap_err();
    assert_eq!(
        (err.kind, err.message.as_str()),
        (ErrorKind::NotFound, "function not found")
    );
    let req = ironflowx_proto::GetFunctionRequest {
        name: "refund".into(),
    };
    let err = f.engine.get_function(&f.admin, req).await.unwrap_err();
    assert_eq!(
        (err.kind, err.message.as_str()),
        (ErrorKind::NotFound, "function not found")
    );
    for id in ["0190b5f0-0000-7000-8000-000000000999", "not-an-id"] {
        let req = GetRunRequest { run_id: id.into() };
        let err = f.engine.get_run(&f.admin, req).await.unwrap_err();
        assert_eq!(
            (err.kind, err.message.as_str()),
            (ErrorKind::NotFound, "run not found")
        );
    }
    f.engine.close().await;
}

/// The Errors row: `list_functions` with a cursor that does not decode, or that another
/// incarnation issued, is `Invalid`; this engine's own continues the list.
#[tokio::test]
async fn a_function_cursor_that_does_not_decode_or_is_another_incarnations_is_invalid() {
    let f = fixture().await;
    let other = fixture().await;
    for (fx, name) in [(&f, "refund"), (&other, "refund")] {
        let put = PutFunctionRequest {
            name: name.into(),
            code: "v1".into(),
            ..PutFunctionRequest::default()
        };
        fx.engine.put_function(&fx.admin, put).await.unwrap();
    }
    let page = page_of(&f, None).await.unwrap();
    let names = |p: &ironflowx_proto::FunctionPage| {
        p.functions
            .iter()
            .map(|f| f.name.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(names(&page), ["charge"]);
    let rest = page_of(&f, page.next.clone()).await.unwrap();
    assert_eq!((names(&rest), rest.next), (vec!["refund".to_owned()], None));
    let foreign = page_of(&other, None).await.unwrap().next.unwrap();
    for cursor in ["not-a-token".to_owned(), foreign] {
        let err = page_of(&f, Some(cursor.clone())).await.unwrap_err();
        assert_eq!(
            (err.kind, err.message.as_str()),
            (ErrorKind::Invalid, "invalid cursor"),
            "{cursor}"
        );
    }
    f.engine.close().await;
    other.engine.close().await;
}

async fn page_of(
    fx: &Fixture,
    cursor: Option<String>,
) -> Result<ironflowx_proto::FunctionPage, OpError> {
    let req = ListFunctionsRequest {
        limit: std::num::NonZeroU32::new(1),
        cursor,
    };
    fx.engine.list_functions(&fx.admin, req).await
}

/// A listed function is the function `GetFunction` reads, every field of it, for a definition with
/// triggers and the whole policy put.
#[tokio::test]
async fn a_listed_function_is_what_get_function_reads() {
    let f = fixture().await;
    let put = PutFunctionRequest {
        name: "refund".into(),
        code: "v7".into(),
        triggers: vec!["order.refunded".into(), "order.cancelled".into()],
        max_attempts: Some(3),
        first_delay_seconds: Some(2),
        max_delay_seconds: Some(40),
    };
    f.engine.put_function(&f.admin, put).await.unwrap();
    let req = ironflowx_proto::GetFunctionRequest {
        name: "refund".into(),
    };
    let got = f.engine.get_function(&f.admin, req).await.unwrap();
    let req = ListFunctionsRequest::default();
    let page = f.engine.list_functions(&f.admin, req).await.unwrap();
    assert_eq!(page.functions.last(), Some(&got));
    assert_eq!(page.functions.len(), 2);
    f.engine.close().await;
}
