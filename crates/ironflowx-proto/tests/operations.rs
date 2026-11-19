//! Fails to compile if `Operations` stops being implementable by a plain adapter, or callable
//! through a generic function.

use std::future::Future;
use std::num::NonZeroU32;
use std::pin::pin;
use std::task::{Context, Poll, Waker};

use ironflowx_core::{ErrorKind, EventId, Timestamp};
use ironflowx_proto::{
    Call, EmitBatchRequest, EmitBatchResponse, EmitRequest, EmitResponse, Event, EventPage,
    GetEventRequest, ListEventsRequest, OpError, Operations,
};

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
        cx: &Call,
        req: EmitBatchRequest,
    ) -> Result<EmitBatchResponse, OpError> {
        let mut items = Vec::new();
        for item in req.items {
            items.push(self.emit(cx, item).await);
        }
        Ok(EmitBatchResponse { items })
    }

    async fn get_event(&self, _cx: &Call, req: GetEventRequest) -> Result<Event, OpError> {
        let event_id: EventId = req.event_id.parse().map_err(|_| OpError {
            kind: ErrorKind::NotFound,
            message: "no such event".into(),
        })?;
        Ok(Event {
            event_id,
            name: "a".into(),
            data: "null".into(),
            metadata: None,
            version: 1,
            idempotency_key: None,
            time: Timestamp::from_millis(0),
        })
    }

    async fn list_events(&self, _cx: &Call, _req: ListEventsRequest) -> Result<EventPage, OpError> {
        Ok(EventPage {
            events: Vec::new(),
            next: None,
        })
    }
}

/// Polls a future that must be ready at once; `Fixed` never awaits anything pending.
fn now<F: Future>(f: F) -> F::Output {
    match pin!(f).poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(out) => out,
        Poll::Pending => panic!("pending"),
    }
}

fn assert_send<T: Send>(t: T) -> T {
    t
}

/// A caller that knows only the trait, as the behavior steps will be.
fn drive<O: Operations>(ops: &O) {
    let cx = Call::anonymous();
    let req = EmitRequest {
        name: "a".into(),
        data: Some("{}".into()),
        metadata: None,
        version: None,
        idempotency_key: Some("k".into()),
    };

    let emitted = now(assert_send(ops.emit(&cx, req.clone()))).unwrap();
    assert!(!emitted.duplicate);

    let batch = now(assert_send(ops.emit_batch(
        &cx,
        EmitBatchRequest {
            items: vec![req.clone(), req],
        },
    )))
    .unwrap();
    assert_eq!(batch.items.len(), 2);

    let event = now(assert_send(ops.get_event(
        &cx,
        GetEventRequest {
            event_id: emitted.event_id.to_string(),
        },
    )))
    .unwrap();
    assert_eq!(event.event_id, emitted.event_id);

    let missing = now(ops.get_event(
        &cx,
        GetEventRequest {
            event_id: "not an id".into(),
        },
    ))
    .unwrap_err();
    assert_eq!(missing.kind, ErrorKind::NotFound);

    let page = now(assert_send(ops.list_events(
        &cx,
        ListEventsRequest {
            limit: NonZeroU32::new(5),
            cursor: None,
        },
    )))
    .unwrap();
    assert!(page.next.is_none());
}

#[test]
fn a_generic_caller_drives_any_adapter() {
    drive(&Fixed);
}
