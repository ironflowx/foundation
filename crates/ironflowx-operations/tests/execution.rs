//! Execution operations through the engine: the fence on every call that names a lease, and a
//! run's end with its event.

use std::collections::BTreeSet;

use ironflowx_cell::ManualClock;
use ironflowx_core::{ErrorKind, Lease, Scope, Timestamp};
use ironflowx_operations::Engine;
use ironflowx_proto::{
    BeginStepRequest, Call, ClaimRunRequest, CompleteRunRequest, CompleteStepRequest,
    CreateKeyRequest, GetRunRequest, ListEventsRequest, OpError, Operations, PutFunctionRequest,
    Run, RunStatus, StartRunRequest, WhoamiRequest,
};

const T: i64 = 1_800_000_000_000;

struct Fixture {
    engine: Engine,
    admin: Call,
    executor: Call,
    other: Call,
    _dir: tempfile::TempDir,
}

async fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let clock = ManualClock::new(Timestamp::from_millis(T));
    let opened = Engine::open(dir.path(), clock, None).await.unwrap();
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
        let req = ClaimRunRequest {
            functions: vec!["charge@v1".into()],
            ..ClaimRunRequest::default()
        };
        let claim = self.engine.claim_run(&self.executor, req).await.unwrap();
        claim.expect("a due run").lease
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
            _ => unreachable!(),
        }
    }
}

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
    for call in ["begin", "complete-step", "complete"] {
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
    assert_eq!(event.emitted_by.to_string(), format!("key:{}", me.key_id));
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
/// function, or run, is `NotFound`.
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
    assert_eq!(run.created_by.to_string(), format!("key:{}", me.key_id));
    assert_eq!((run.execution, run.steps, run.lease_until), (0, 0, None));
    let req = StartRunRequest {
        function: "refund".into(),
    };
    let err = f.engine.start_run(&f.admin, req).await.unwrap_err();
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
