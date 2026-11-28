//! An append whose outcome is lost, through the engine (the Errors table; D11): the emit is
//! `Unknown`, then every event call is `Unavailable`, until a reopen, after which a keyed retry
//! returns the original event if it landed, else stores it once. Its own test binary, since the
//! fault hook is process-wide.

use std::pin::pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Waker};

use ironflowx_cell::{FaultPoint, ManualClock, set_fault_hook};
use ironflowx_core::{ErrorKind, Secret, Timestamp};
use ironflowx_operations::Engine;
use ironflowx_proto::{
    Call, EmitBatchRequest, EmitRequest, GetEventRequest, ListEventsRequest, OpError, Operations,
};

/// Whether the append is durable before its outcome is lost.
static LANDED: AtomicBool = AtomicBool::new(false);
/// Whether the next append is the one to lose.
static ARMED: AtomicBool = AtomicBool::new(false);

/// One test at a time loses an append.
static ALONE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Kills the writer during an append, so its reply is lost.
fn lose_the_append(point: FaultPoint) {
    let at = if LANDED.load(Ordering::SeqCst) {
        FaultPoint::AfterFsync
    } else {
        FaultPoint::BeforeAppend
    };
    if point == at && ARMED.swap(false, Ordering::SeqCst) {
        panic!("the append's outcome is lost");
    }
}

fn emit() -> EmitRequest {
    EmitRequest {
        name: "order.placed".into(),
        data: None,
        metadata: None,
        version: None,
        idempotency_key: Some("k".into()),
    }
}

const fn first_page() -> ListEventsRequest {
    ListEventsRequest {
        limit: None,
        cursor: None,
    }
}

fn told<T: std::fmt::Debug>(answer: Result<T, OpError>) -> (ErrorKind, bool, Option<String>) {
    let err = answer.expect_err("a failed ledger refuses the call");
    (err.kind, err.kind.retryable(), err.hint)
}

/// Whether `f` is ready when polled once.
fn ready<F: Future>(f: std::pin::Pin<&mut F>) -> bool {
    f.poll(&mut Context::from_waker(Waker::noop())).is_ready()
}

#[tokio::test]
async fn an_unknown_emit_is_settled_by_a_keyed_retry_after_reopening_the_engine() {
    let _alone = ALONE.lock().await;
    set_fault_hook(lose_the_append);
    for landed in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::new(Timestamp::from_millis(1_800_000_000_000));
        let opened = Engine::open(dir.path(), clock.clone(), None).await.unwrap();
        let (engine, key) = (opened.engine, opened.minted.unwrap());
        let cx = Call::with_key(Secret::new(key.expose().to_owned()));

        LANDED.store(landed, Ordering::SeqCst);
        ARMED.store(true, Ordering::SeqCst);
        let (kind, retryable, hint) = told(engine.emit(&cx, emit()).await);
        // The fault is signalled before the answer.
        assert!(ready(pin!(engine.faulted())));
        assert_eq!((kind, retryable), (ErrorKind::Unknown, false));
        assert_eq!(
            hint.as_deref(),
            Some("reopen the engine, then retry with the same idempotency key")
        );

        let unavailable = (
            ErrorKind::Unavailable,
            true,
            Some("reopen the engine, then retry".to_owned()),
        );
        let batch = EmitBatchRequest {
            items: vec![emit()],
        };
        let get = GetEventRequest {
            event_id: "019a8f3c-7e21-7b4a-9c3d-5e6f7a8b9c0d".into(),
        };
        assert_eq!(told(engine.emit(&cx, emit()).await), unavailable);
        assert_eq!(told(engine.emit_batch(&cx, batch).await), unavailable);
        assert_eq!(told(engine.get_event(&cx, get).await), unavailable);
        let listed = engine.list_events(&cx, first_page()).await;
        assert_eq!(told(listed), unavailable);
        engine.close().await;

        let opened = Engine::open(dir.path(), clock, None).await.unwrap();
        let engine = opened.engine;
        let before = engine.list_events(&cx, first_page()).await.unwrap().events;
        assert_eq!(before.len(), usize::from(landed));
        let retry = engine.emit(&cx, emit()).await.unwrap();
        assert_eq!(retry.duplicate, landed);
        let after = engine.list_events(&cx, first_page()).await.unwrap().events;
        let ids: Vec<_> = after.iter().map(|e| e.event_id).collect();
        assert_eq!(ids, [retry.event_id]);
        engine.close().await;
    }
}

/// The hint fits the request, and the message says nothing it does not.
#[tokio::test]
async fn an_unknown_emits_hint_fits_its_keys() {
    let _alone = ALONE.lock().await;
    set_fault_hook(lose_the_append);
    LANDED.store(true, Ordering::SeqCst);
    let unkeyed = || EmitRequest {
        idempotency_key: None,
        ..emit()
    };
    let batch = "reopen the engine, then retry unchanged only if every item had an idempotency \
                 key and keeps it; else check first what landed";
    for (items, hint) in [
        (
            Err(emit()),
            "reopen the engine, then retry with the same idempotency key",
        ),
        (
            Err(unkeyed()),
            "reopen the engine, then check first what landed: a retry may store a second event",
        ),
        (Ok(vec![emit()]), batch),
        (Ok(vec![emit(), unkeyed()]), batch),
        (Ok(vec![unkeyed()]), batch),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::new(Timestamp::from_millis(1_800_000_000_000));
        let opened = Engine::open(dir.path(), clock, None).await.unwrap();
        let (engine, key) = (opened.engine, opened.minted.unwrap());
        let cx = Call::with_key(key);
        ARMED.store(true, Ordering::SeqCst);
        let answer = match items {
            Ok(items) => {
                let batch = EmitBatchRequest { items };
                engine.emit_batch(&cx, batch).await.map(drop)
            }
            Err(one) => engine.emit(&cx, one).await.map(drop),
        };
        let unknown = OpError {
            kind: ErrorKind::Unknown,
            message: "the outcome is unknown".into(),
            hint: Some(hint.into()),
        };
        assert_eq!(answer, Err(unknown));
        engine.close().await;
    }
}

/// Each execution call's lost answer has the hint of what is true of it: a put again replaces
/// whatever is current, a start again starts a second run, a lost claim's run stays claimed, and a
/// call on a lease repeats no work.
#[tokio::test]
async fn an_unknown_execution_calls_hint_says_what_a_retry_does() {
    use ironflowx_proto::{
        BeginStepRequest, ClaimRunRequest, CompleteRunRequest, CompleteStepRequest,
        PutFunctionRequest, StartRunRequest,
    };
    let _alone = ALONE.lock().await;
    set_fault_hook(lose_the_append);
    LANDED.store(false, Ordering::SeqCst);
    let put = || PutFunctionRequest {
        name: "charge".into(),
        code: "v1".into(),
        ..PutFunctionRequest::default()
    };
    let lease = "reopen the engine, then send the same call again with the same lease: it \
                 repeats no work";
    for call in [
        "put",
        "start",
        "claim",
        "begin-step",
        "complete-step",
        "complete",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::new(Timestamp::from_millis(1_800_000_000_000));
        let opened = Engine::open(dir.path(), clock, None).await.unwrap();
        let (engine, key) = (opened.engine, opened.minted.unwrap());
        let cx = Call::with_key(key);
        engine.put_function(&cx, put()).await.unwrap();
        let start = || StartRunRequest {
            function: "charge".into(),
        };
        engine.start_run(&cx, start()).await.unwrap();
        let claim = || ClaimRunRequest {
            functions: vec!["charge@v1".into()],
            ..ClaimRunRequest::default()
        };
        // Each step call needs what the one before it recorded.
        let mut lease_text = String::new();
        if call != "put" && call != "start" && call != "claim" {
            let claimed = engine.claim_run(&cx, claim()).await.unwrap();
            lease_text = claimed.unwrap().lease.to_string();
        }
        let begin = BeginStepRequest {
            lease: lease_text.clone(),
            position: 1,
            name: "pay".into(),
            ..BeginStepRequest::default()
        };
        if call == "complete-step" {
            engine.begin_step(&cx, begin.clone()).await.unwrap();
        }
        // A changed definition, so the put appends.
        let changed = PutFunctionRequest {
            code: "v2".into(),
            ..put()
        };
        ARMED.store(true, Ordering::SeqCst);
        let (answer, hint) = match call {
            "put" => (
                engine.put_function(&cx, changed).await.map(drop),
                "reopen the engine, then put again only if this definition should still replace \
                 the current one: another put may have landed since",
            ),
            "start" => (
                engine.start_run(&cx, start()).await.map(drop),
                "reopen the engine, then check first what landed: a retry starts a second run",
            ),
            "claim" => (
                engine.claim_run(&cx, claim()).await.map(drop),
                "reopen the engine, then claim again: until leases expire (a later release), the \
                 run of a lost claim is not claimed again",
            ),
            "begin-step" => (engine.begin_step(&cx, begin).await.map(drop), lease),
            "complete-step" => {
                let req = CompleteStepRequest {
                    lease: lease_text,
                    position: 1,
                    output: None,
                };
                (engine.complete_step(&cx, req).await.map(drop), lease)
            }
            _ => {
                let req = CompleteRunRequest {
                    lease: lease_text,
                    steps: 0,
                    output: None,
                };
                (engine.complete_run(&cx, req).await.map(drop), lease)
            }
        };
        let unknown = OpError {
            kind: ErrorKind::Unknown,
            message: "the outcome is unknown".into(),
            hint: Some(hint.into()),
        };
        assert_eq!(answer, Err(unknown), "{call}");
        engine.close().await;
    }
}
