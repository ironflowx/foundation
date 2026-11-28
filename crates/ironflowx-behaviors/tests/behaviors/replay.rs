//! The tracer's replay, in place of EXE-010 until a run can be claimed again (#223): one execution
//! records step 1, asks about it again and is answered its recorded output byte for byte; then it
//! records step 2 and completes the run. On the engine, and through the door.

use std::collections::BTreeSet;

use ironflowx_cell::ManualClock;
use ironflowx_core::{Scope, Timestamp};
use ironflowx_operations::Engine;
use ironflowx_proto::{
    BeginStepRequest, BeginStepResponse, Call, ClaimRunRequest, CompleteRunRequest,
    CompleteStepRequest, CompleteStepResponse, CreateKeyRequest, ListEventsRequest, Operations,
    PutFunctionRequest, RunStatus, StartRunRequest,
};

use super::adapter::Adapter;

/// Two spaces, and a number above 2^53 that a float would round.
const OUTPUT: &[u8] = br#"{"id":  9007199254740993}"#;

pub async fn check(remote: bool) {
    let dir = tempfile::tempdir().unwrap();
    let (ops, key) = if remote {
        Adapter::served(&dir.path().join("data")).await
    } else {
        let clock = ManualClock::new(Timestamp::from_millis(1_800_000_000_000));
        let opened = Engine::open(dir.path(), clock, None).await.unwrap();
        let key = opened.minted.unwrap().expose().to_owned();
        (Adapter::Engine(opened.engine), key)
    };
    let admin = Call::with_key(ironflowx_core::Secret::new(key));
    let req = CreateKeyRequest {
        scopes: BTreeSet::from([Scope::Execute]),
        ..CreateKeyRequest::default()
    };
    let executor = Call::with_key(ops.create_key(&admin, req).await.unwrap().key);
    let put = PutFunctionRequest {
        name: "charge".into(),
        code: "v1".into(),
        ..PutFunctionRequest::default()
    };
    ops.put_function(&admin, put).await.unwrap();
    let start = StartRunRequest {
        function: "charge".into(),
    };
    let run_id = ops.start_run(&admin, start).await.unwrap().run_id;
    let claim = ClaimRunRequest {
        functions: vec!["charge@v1".into()],
        wait_seconds: Some(0),
        ..ClaimRunRequest::default()
    };
    let claim = ops
        .claim_run(&executor, claim)
        .await
        .unwrap()
        .expect("a due run");
    assert_eq!(claim.lease.run, run_id);
    let lease = claim.lease.to_string();
    let begin = |position, name: &str| BeginStepRequest {
        lease: lease.clone(),
        position,
        name: name.into(),
        ..BeginStepRequest::default()
    };
    let complete = |position, output: &[u8]| CompleteStepRequest {
        lease: lease.clone(),
        position,
        output: Some(output.to_vec()),
    };
    let run = BeginStepResponse::Run { attempt: 1 };
    assert_eq!(
        ops.begin_step(&executor, begin(1, "reserve")).await,
        Ok(run.clone())
    );
    let recorded = CompleteStepResponse::Completed {
        output: OUTPUT.to_vec(),
    };
    let answer = ops.complete_step(&executor, complete(1, OUTPUT)).await;
    assert_eq!(answer, Ok(recorded));
    let get = || {
        ops.get_run(
            &admin,
            ironflowx_proto::GetRunRequest {
                run_id: run_id.to_string(),
            },
        )
    };
    let events = || {
        ops.list_events(
            &admin,
            ListEventsRequest {
                limit: None,
                cursor: None,
            },
        )
    };
    let before = (get().await.unwrap(), events().await.unwrap());
    let replayed = ops.begin_step(&executor, begin(1, "reserve")).await;
    let BeginStepResponse::Completed { output } = replayed.unwrap() else {
        panic!("a recorded step is answered completed");
    };
    assert_eq!(output, OUTPUT, "byte for byte");
    // Nothing changed: the ledger's own test shows nothing was appended.
    assert_eq!((get().await.unwrap(), events().await.unwrap()), before);
    assert_eq!(ops.begin_step(&executor, begin(2, "pay")).await, Ok(run));
    ops.complete_step(&executor, complete(2, b"2"))
        .await
        .unwrap();
    let end = CompleteRunRequest {
        lease: lease.clone(),
        steps: 2,
        output: None,
    };
    let end = ops.complete_run(&executor, end).await.unwrap();
    assert_eq!((end.run_id, end.status), (run_id, RunStatus::Completed));
    let read = get().await.unwrap();
    assert_eq!(
        (read.status, read.steps, read.step),
        (RunStatus::Completed, 2, None)
    );
    ops.close().await;
}
