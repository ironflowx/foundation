//! Fails to compile if `Operations` stops being implementable by a plain adapter, or callable
//! through a generic function.

use std::future::Future;
use std::num::NonZeroU32;
use std::pin::pin;
use std::sync::Mutex;
use std::task::{Context, Poll, Waker};

use std::collections::BTreeSet;

use ironflowx_core::{Address, ErrorKind, KeyOrigin, Lease, Scope, Secret, Timestamp};
use ironflowx_proto::{
    BeginStepRequest, BeginStepResponse, Call, Claim, ClaimRunRequest, CompleteRunRequest,
    CompleteStepRequest, CompleteStepResponse, CreateKeyRequest, CreatedKey, EmitBatchRequest,
    EmitBatchResponse, EmitRequest, EmitResponse, Environment, Event, EventPage, Function,
    GetEventRequest, GetFunctionRequest, GetRunRequest, KeyInfo, KeyPage, KeyStatus,
    ListEnvironmentsRequest, ListEventsRequest, ListKeysRequest, OpError, Operation, Operations,
    PutFunctionRequest, PutFunctionResponse, RevokeKeyRequest, Run, RunEnd, RunStatus,
    StartRunRequest, StartRunResponse, Whoami, WhoamiRequest, required_scope,
};

const KEY: &str =
    "ixk_0190b5f0-0000-7000-8000-000000000001_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

const ID: &str = "0190b5f0-0000-7000-8000-000000000000";

/// Answers every call from fixed values; no logic.
struct Fixed;

impl Operations for Fixed {
    async fn emit(&self, _cx: &Call, _req: EmitRequest) -> Result<EmitResponse, OpError> {
        Ok(EmitResponse {
            event_id: ID.parse().unwrap(),
            duplicate: false,
        })
    }

    async fn emit_batch(
        &self,
        _cx: &Call,
        _req: EmitBatchRequest,
    ) -> Result<EmitBatchResponse, OpError> {
        Ok(EmitBatchResponse {
            items: vec![Err(OpError {
                kind: ErrorKind::Invalid,
                message: "refused".into(),
                hint: None,
            })],
        })
    }

    async fn get_event(&self, _cx: &Call, _req: GetEventRequest) -> Result<Event, OpError> {
        Ok(Event {
            event_id: ID.parse().unwrap(),
            name: "a".into(),
            data: b"null".to_vec(),
            metadata: None,
            version: 1,
            idempotency_key: None,
            time: Timestamp::from_millis(0),
            emitted_by: "embedder:test".parse().unwrap(),
        })
    }

    async fn list_events(&self, _cx: &Call, _req: ListEventsRequest) -> Result<EventPage, OpError> {
        Ok(EventPage {
            events: Vec::new(),
            next: None,
        })
    }

    async fn whoami(&self, _cx: &Call, _req: WhoamiRequest) -> Result<Whoami, OpError> {
        Ok(Whoami {
            key_id: KEY[4..40].parse().unwrap(),
            scopes: BTreeSet::from([Scope::Read]),
            environment: Address::new("default", "default"),
            expires_at: None,
        })
    }

    async fn list_environments(
        &self,
        _cx: &Call,
        _req: ListEnvironmentsRequest,
    ) -> Result<Vec<Environment>, OpError> {
        Ok(Vec::new())
    }

    async fn create_key(&self, _cx: &Call, req: CreateKeyRequest) -> Result<CreatedKey, OpError> {
        Ok(created(req))
    }

    async fn list_keys(&self, _cx: &Call, _req: ListKeysRequest) -> Result<KeyPage, OpError> {
        Ok(KeyPage {
            keys: vec![created(CreateKeyRequest::default()).info],
            next: None,
        })
    }

    async fn revoke_key(&self, _cx: &Call, _req: RevokeKeyRequest) -> Result<(), OpError> {
        Ok(())
    }

    async fn put_function(
        &self,
        _cx: &Call,
        req: PutFunctionRequest,
    ) -> Result<PutFunctionResponse, OpError> {
        Ok(PutFunctionResponse {
            function: function(&req.name),
            changed: true,
        })
    }

    async fn get_function(&self, _cx: &Call, req: GetFunctionRequest) -> Result<Function, OpError> {
        Ok(function(&req.name))
    }

    async fn start_run(
        &self,
        _cx: &Call,
        _req: StartRunRequest,
    ) -> Result<StartRunResponse, OpError> {
        Ok(StartRunResponse {
            run_id: ID.parse().unwrap(),
            duplicate: false,
        })
    }

    async fn get_run(&self, _cx: &Call, req: GetRunRequest) -> Result<Run, OpError> {
        Ok(Run {
            run_id: req.run_id.parse().unwrap(),
            function: "charge".into(),
            code: "v1".into(),
            status: RunStatus::Queued,
            input: Some(b"null".to_vec()),
            created_at: Timestamp::from_millis(0),
            created_by: "embedder:test".parse().unwrap(),
            due_at: Some(Timestamp::from_millis(0)),
            execution: 0,
            lease_until: None,
            steps: 0,
            step: None,
            output: None,
            ended_at: None,
        })
    }

    async fn claim_run(&self, _cx: &Call, _req: ClaimRunRequest) -> Result<Option<Claim>, OpError> {
        Ok(Some(Claim {
            lease: Lease {
                run: ID.parse().unwrap(),
                execution: 1,
            },
            function: "charge".into(),
            code: "v1".into(),
            input: Some(b"null".to_vec()),
            lease_until: Timestamp::from_millis(60_000),
            renew_after_seconds: 20,
        }))
    }

    async fn begin_step(
        &self,
        _cx: &Call,
        _req: BeginStepRequest,
    ) -> Result<BeginStepResponse, OpError> {
        Ok(BeginStepResponse::Run { attempt: 1 })
    }

    async fn complete_step(
        &self,
        _cx: &Call,
        req: CompleteStepRequest,
    ) -> Result<CompleteStepResponse, OpError> {
        Ok(CompleteStepResponse::Completed {
            output: req.output.unwrap_or_default(),
        })
    }

    async fn complete_run(&self, _cx: &Call, req: CompleteRunRequest) -> Result<RunEnd, OpError> {
        let lease: Lease = req.lease.parse().unwrap();
        Ok(RunEnd {
            run_id: lease.run,
            status: RunStatus::Completed,
        })
    }
}

fn function(name: &str) -> Function {
    Function {
        name: name.to_owned(),
        code: "v1".into(),
        triggers: Vec::new(),
        max_attempts: None,
        first_delay_seconds: None,
        max_delay_seconds: None,
        put_at: Timestamp::from_millis(0),
        put_by: "embedder:test".parse().unwrap(),
    }
}

fn created(req: CreateKeyRequest) -> CreatedKey {
    CreatedKey {
        key: Secret::new(KEY.to_owned()),
        info: KeyInfo {
            key_id: KEY[4..40].parse().unwrap(),
            label: req.label,
            scopes: req.scopes,
            origin: KeyOrigin::Created,
            created_by: None,
            created_at: Timestamp::from_millis(0),
            expires_at: req.expires_at,
            status: KeyStatus::Active,
        },
    }
}

/// Records each call's [`Operation`], then answers as `Fixed` does.
#[derive(Default)]
struct Recording(Mutex<Vec<Operation>>);

impl Recording {
    fn record(&self, op: Operation) {
        self.0.lock().unwrap().push(op);
    }
}

impl Operations for Recording {
    async fn emit(&self, cx: &Call, req: EmitRequest) -> Result<EmitResponse, OpError> {
        self.record(Operation::Emit);
        Fixed.emit(cx, req).await
    }

    async fn emit_batch(
        &self,
        cx: &Call,
        req: EmitBatchRequest,
    ) -> Result<EmitBatchResponse, OpError> {
        self.record(Operation::EmitBatch);
        Fixed.emit_batch(cx, req).await
    }

    async fn get_event(&self, cx: &Call, req: GetEventRequest) -> Result<Event, OpError> {
        self.record(Operation::GetEvent);
        Fixed.get_event(cx, req).await
    }

    async fn list_events(&self, cx: &Call, req: ListEventsRequest) -> Result<EventPage, OpError> {
        self.record(Operation::ListEvents);
        Fixed.list_events(cx, req).await
    }

    async fn whoami(&self, cx: &Call, req: WhoamiRequest) -> Result<Whoami, OpError> {
        self.record(Operation::Whoami);
        Fixed.whoami(cx, req).await
    }

    async fn list_environments(
        &self,
        cx: &Call,
        req: ListEnvironmentsRequest,
    ) -> Result<Vec<Environment>, OpError> {
        self.record(Operation::ListEnvironments);
        Fixed.list_environments(cx, req).await
    }

    async fn create_key(&self, cx: &Call, req: CreateKeyRequest) -> Result<CreatedKey, OpError> {
        self.record(Operation::CreateKey);
        Fixed.create_key(cx, req).await
    }

    async fn list_keys(&self, cx: &Call, req: ListKeysRequest) -> Result<KeyPage, OpError> {
        self.record(Operation::ListKeys);
        Fixed.list_keys(cx, req).await
    }

    async fn revoke_key(&self, cx: &Call, req: RevokeKeyRequest) -> Result<(), OpError> {
        self.record(Operation::RevokeKey);
        Fixed.revoke_key(cx, req).await
    }

    async fn put_function(
        &self,
        cx: &Call,
        req: PutFunctionRequest,
    ) -> Result<PutFunctionResponse, OpError> {
        self.record(Operation::PutFunction);
        Fixed.put_function(cx, req).await
    }

    async fn get_function(&self, cx: &Call, req: GetFunctionRequest) -> Result<Function, OpError> {
        self.record(Operation::GetFunction);
        Fixed.get_function(cx, req).await
    }

    async fn start_run(
        &self,
        cx: &Call,
        req: StartRunRequest,
    ) -> Result<StartRunResponse, OpError> {
        self.record(Operation::StartRun);
        Fixed.start_run(cx, req).await
    }

    async fn get_run(&self, cx: &Call, req: GetRunRequest) -> Result<Run, OpError> {
        self.record(Operation::GetRun);
        Fixed.get_run(cx, req).await
    }

    async fn claim_run(&self, cx: &Call, req: ClaimRunRequest) -> Result<Option<Claim>, OpError> {
        self.record(Operation::ClaimRun);
        Fixed.claim_run(cx, req).await
    }

    async fn begin_step(
        &self,
        cx: &Call,
        req: BeginStepRequest,
    ) -> Result<BeginStepResponse, OpError> {
        self.record(Operation::BeginStep);
        Fixed.begin_step(cx, req).await
    }

    async fn complete_step(
        &self,
        cx: &Call,
        req: CompleteStepRequest,
    ) -> Result<CompleteStepResponse, OpError> {
        self.record(Operation::CompleteStep);
        Fixed.complete_step(cx, req).await
    }

    async fn complete_run(&self, cx: &Call, req: CompleteRunRequest) -> Result<RunEnd, OpError> {
        self.record(Operation::CompleteRun);
        Fixed.complete_run(cx, req).await
    }
}

/// Polls a future that must be ready at once; `Fixed` never awaits anything pending.
fn ready<F: Future>(f: F) -> F::Output {
    match pin!(f).poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(out) => out,
        Poll::Pending => panic!("pending"),
    }
}

fn assert_send<T: Send>(t: T) -> T {
    t
}

/// A caller that knows only the trait, as the behavior steps will be. Calls each method once.
fn drive<O: Operations>(ops: &O) {
    let cx = Call::anonymous();
    let req = EmitRequest {
        name: "a".into(),
        data: Some(b"{}".to_vec()),
        metadata: None,
        version: None,
        idempotency_key: Some("k".into()),
    };

    let emitted = ready(assert_send(ops.emit(&cx, req.clone()))).unwrap();
    assert!(!emitted.duplicate);

    let batch = ready(assert_send(ops.emit_batch(
        &cx,
        EmitBatchRequest {
            items: vec![req.clone(), req],
        },
    )))
    .unwrap();
    assert_eq!(
        batch.items[0].as_ref().unwrap_err().kind,
        ErrorKind::Invalid
    );

    let event = ready(assert_send(ops.get_event(
        &cx,
        GetEventRequest {
            event_id: emitted.event_id.to_string(),
        },
    )))
    .unwrap();
    assert_eq!(event.event_id, emitted.event_id);

    let page = ready(assert_send(ops.list_events(
        &cx,
        ListEventsRequest {
            limit: NonZeroU32::new(5),
            cursor: None,
        },
    )))
    .unwrap();
    assert!(page.next.is_none());

    ready(assert_send(ops.whoami(&cx, WhoamiRequest {}))).unwrap();
    let envs = ops.list_environments(&cx, ListEnvironmentsRequest {});
    ready(assert_send(envs)).unwrap();

    let key = ready(assert_send(ops.create_key(
        &cx,
        CreateKeyRequest {
            scopes: BTreeSet::new(),
            expires_at: None,
            label: Some("agent-7".into()),
        },
    )))
    .unwrap();
    assert_eq!(key.info.label.as_deref(), Some("agent-7"));

    let listed = ready(assert_send(ops.list_keys(
        &cx,
        ListKeysRequest {
            limit: NonZeroU32::new(100),
            cursor: None,
        },
    )))
    .unwrap();
    assert_eq!(listed.keys.len(), 1);
    let revoked = ops.revoke_key(
        &cx,
        RevokeKeyRequest {
            key_id: listed.keys[0].key_id.to_string(),
        },
    );
    ready(assert_send(revoked)).unwrap();

    let put = PutFunctionRequest {
        name: "charge".into(),
        code: "v1".into(),
        ..PutFunctionRequest::default()
    };
    let put = ready(assert_send(ops.put_function(&cx, put))).unwrap();
    assert_eq!(put.function.name, "charge");
    let got = ops.get_function(
        &cx,
        GetFunctionRequest {
            name: "charge".into(),
        },
    );
    assert_eq!(ready(assert_send(got)).unwrap().name, "charge");
    let started = ops.start_run(
        &cx,
        StartRunRequest {
            function: "charge".into(),
        },
    );
    let started = ready(assert_send(started)).unwrap();
    let run = ops.get_run(
        &cx,
        GetRunRequest {
            run_id: started.run_id.to_string(),
        },
    );
    assert_eq!(ready(assert_send(run)).unwrap().run_id, started.run_id);
    let claim = ClaimRunRequest {
        functions: vec!["charge@v1".into()],
        ..ClaimRunRequest::default()
    };
    let claim = ready(assert_send(ops.claim_run(&cx, claim))).unwrap();
    let lease = claim.expect("a claim").lease.to_string();
    let begin = BeginStepRequest {
        lease: lease.clone(),
        position: 1,
        name: "reserve".into(),
        ..BeginStepRequest::default()
    };
    let begun = ready(assert_send(ops.begin_step(&cx, begin))).unwrap();
    assert_eq!(begun, BeginStepResponse::Run { attempt: 1 });
    let complete = CompleteStepRequest {
        lease: lease.clone(),
        position: 1,
        output: Some(b"1".to_vec()),
    };
    let completed = ready(assert_send(ops.complete_step(&cx, complete))).unwrap();
    assert_eq!(
        completed,
        CompleteStepResponse::Completed {
            output: b"1".to_vec()
        }
    );
    let end = CompleteRunRequest {
        lease,
        steps: 1,
        output: None,
    };
    let end = ready(assert_send(ops.complete_run(&cx, end))).unwrap();
    assert_eq!(end.run_id, started.run_id);
}

/// The scope table (architecture.md, "Keys"): `whoami` needs only a valid key.
#[test]
fn each_operation_requires_its_scope() {
    for (op, scope) in [
        (Operation::Emit, Some(Scope::Write)),
        (Operation::EmitBatch, Some(Scope::Write)),
        (Operation::GetEvent, Some(Scope::Read)),
        (Operation::ListEvents, Some(Scope::Read)),
        (Operation::ListEnvironments, Some(Scope::Read)),
        (Operation::Whoami, None),
        (Operation::CreateKey, Some(Scope::Admin)),
        (Operation::ListKeys, Some(Scope::Admin)),
        (Operation::RevokeKey, Some(Scope::Admin)),
        (Operation::PutFunction, Some(Scope::Write)),
        (Operation::GetFunction, Some(Scope::Read)),
        (Operation::StartRun, Some(Scope::Write)),
        (Operation::GetRun, Some(Scope::Read)),
        (Operation::ClaimRun, Some(Scope::Execute)),
        (Operation::BeginStep, Some(Scope::Execute)),
        (Operation::CompleteStep, Some(Scope::Execute)),
        (Operation::CompleteRun, Some(Scope::Execute)),
    ] {
        assert_eq!(required_scope(op), scope, "{op:?}");
    }
}

/// Each method of the trait is one operation (SRV-2).
#[test]
fn one_call_of_each_method_is_all_the_operations() {
    let ops = Recording::default();
    drive(&ops);
    assert_eq!(ops.0.into_inner().unwrap(), Operation::ALL);
}

/// One RPC each.
#[test]
fn each_operation_has_its_rpc() {
    let rpcs: Vec<_> = Operation::ALL.iter().map(|op| (*op, op.rpc())).collect();
    assert_eq!(
        rpcs,
        [
            (Operation::Emit, "ironflowx.v1.EventService/Emit"),
            (Operation::EmitBatch, "ironflowx.v1.EventService/EmitBatch"),
            (Operation::GetEvent, "ironflowx.v1.EventService/GetEvent"),
            (
                Operation::ListEvents,
                "ironflowx.v1.EventService/ListEvents"
            ),
            (Operation::Whoami, "ironflowx.v1.IdentityService/Whoami"),
            (
                Operation::ListEnvironments,
                "ironflowx.v1.EnvironmentService/ListEnvironments"
            ),
            (Operation::CreateKey, "ironflowx.v1.KeyService/CreateKey"),
            (Operation::ListKeys, "ironflowx.v1.KeyService/ListKeys"),
            (Operation::RevokeKey, "ironflowx.v1.KeyService/RevokeKey"),
            (
                Operation::PutFunction,
                "ironflowx.v1.FunctionService/PutFunction"
            ),
            (
                Operation::GetFunction,
                "ironflowx.v1.FunctionService/GetFunction"
            ),
            (Operation::StartRun, "ironflowx.v1.RunService/StartRun"),
            (Operation::GetRun, "ironflowx.v1.RunService/GetRun"),
            (
                Operation::ClaimRun,
                "ironflowx.v1.ExecutionService/ClaimRun"
            ),
            (
                Operation::BeginStep,
                "ironflowx.v1.ExecutionService/BeginStep"
            ),
            (
                Operation::CompleteStep,
                "ironflowx.v1.ExecutionService/CompleteStep"
            ),
            (
                Operation::CompleteRun,
                "ironflowx.v1.ExecutionService/CompleteRun"
            ),
        ]
    );
}

#[test]
fn every_operation_but_the_reads_changes_state() {
    let changing: Vec<_> = Operation::ALL
        .iter()
        .copied()
        .filter(|op| op.changes_state())
        .collect();
    assert_eq!(
        changing,
        [
            Operation::Emit,
            Operation::EmitBatch,
            Operation::CreateKey,
            Operation::RevokeKey,
            Operation::PutFunction,
            Operation::StartRun,
            Operation::ClaimRun,
            Operation::BeginStep,
            Operation::CompleteStep,
            Operation::CompleteRun,
        ]
    );
}

#[test]
fn a_created_key_prints_no_key() {
    let shown = format!(
        "{:?}",
        created(CreateKeyRequest {
            scopes: BTreeSet::new(),
            expires_at: None,
            label: None,
        })
    );
    assert!(!shown.contains(&KEY[41..]), "{shown}");
    assert!(shown.contains("CreatedKey"), "{shown}");
}

#[test]
fn an_op_error_is_a_std_error_and_a_call_prints_no_field() {
    let boxed: Box<dyn std::error::Error + Send + Sync> = Box::new(OpError {
        kind: ErrorKind::NotFound,
        message: "event not found".into(),
        hint: None,
    });
    assert_eq!(boxed.to_string(), "NotFound: event not found");
    assert_eq!(format!("{:?}", Call::anonymous()), "Call { .. }");
}

#[test]
fn a_generic_caller_drives_any_adapter() {
    drive(&Fixed);
}
