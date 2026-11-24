//! An append whose outcome is lost, through the engine (the Errors table; D11): the emit is
//! `Unknown`, then every event call is `Unavailable`, until a reopen, after which a keyed retry
//! returns the original event if it landed, else stores it once. Its own test binary, since the
//! fault hook is process-wide.

use std::sync::atomic::{AtomicBool, Ordering};

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

#[tokio::test]
async fn an_unknown_emit_is_settled_by_a_keyed_retry_after_reopening_the_engine() {
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
