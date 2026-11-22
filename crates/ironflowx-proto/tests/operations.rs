//! Fails to compile if `Operations` stops being implementable by a plain adapter, or callable
//! through a generic function.

use std::future::Future;
use std::num::NonZeroU32;
use std::pin::pin;
use std::task::{Context, Poll, Waker};

use std::collections::BTreeSet;

use ironflowx_core::{ErrorKind, KeyOrigin, Scope, Secret, Timestamp};
use ironflowx_proto::{
    Call, CreateKeyRequest, CreatedKey, EmitBatchRequest, EmitBatchResponse, EmitRequest,
    EmitResponse, Event, EventPage, GetEventRequest, KeyInfo, KeyPage, KeyStatus,
    ListEventsRequest, ListKeysRequest, OpError, Operation, Operations, RevokeKeyRequest,
    required_scope,
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

/// A caller that knows only the trait, as the behavior steps will be.
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
    ] {
        assert_eq!(required_scope(op), scope, "{op:?}");
    }
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
