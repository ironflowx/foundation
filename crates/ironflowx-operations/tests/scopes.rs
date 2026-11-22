//! TEN-1's matrix: every operation against no key, a malformed key and each single-scope key.
//! A key holding the operation's scope gets past admission; any other is refused before the
//! handler runs.

use std::collections::BTreeSet;

use ironflowx_cell::ManualClock;
use ironflowx_core::{ErrorKind, Scope, Secret, Timestamp};
use ironflowx_operations::Engine;
use ironflowx_proto::{
    Call, CreateKeyRequest, EmitBatchRequest, EmitRequest, GetEventRequest,
    ListEnvironmentsRequest, ListEventsRequest, OpError, Operation, WhoamiRequest, required_scope,
};

/// Every operation. A new one fails to compile in `call` until it joins this list too.
const ALL: [Operation; 7] = [
    Operation::Emit,
    Operation::EmitBatch,
    Operation::GetEvent,
    Operation::ListEvents,
    Operation::Whoami,
    Operation::ListEnvironments,
    Operation::CreateKey,
];

fn emit() -> EmitRequest {
    EmitRequest {
        name: "order.placed".into(),
        data: None,
        metadata: None,
        version: None,
        idempotency_key: None,
    }
}

/// Calls `op` with `cx`, keeping only whether it failed and how.
async fn call(engine: &Engine, op: Operation, cx: &Call, event_id: &str) -> Result<(), OpError> {
    match op {
        Operation::Emit => engine.emit(cx, emit()).await.map(drop),
        Operation::EmitBatch => {
            let req = EmitBatchRequest {
                items: vec![emit()],
            };
            engine.emit_batch(cx, req).await.map(drop)
        }
        Operation::GetEvent => {
            let req = GetEventRequest {
                event_id: event_id.into(),
            };
            engine.get_event(cx, req).await.map(drop)
        }
        Operation::ListEvents => {
            let req = ListEventsRequest {
                limit: None,
                cursor: None,
            };
            engine.list_events(cx, req).await.map(drop)
        }
        Operation::Whoami => engine.whoami(cx, WhoamiRequest {}).await.map(drop),
        Operation::ListEnvironments => engine
            .list_environments(cx, ListEnvironmentsRequest {})
            .await
            .map(drop),
        Operation::CreateKey => {
            // Within an admin-only key, so admission alone decides.
            let req = CreateKeyRequest {
                scopes: BTreeSet::from([Scope::Admin]),
                ..CreateKeyRequest::default()
            };
            engine.create_key(cx, req).await.map(drop)
        }
    }
}

#[tokio::test]
async fn every_operation_admits_exactly_the_keys_holding_its_scope() {
    let dir = tempfile::tempdir().unwrap();
    let clock = ManualClock::new(Timestamp::from_millis(1_800_000_000_000));
    let opened = Engine::open(dir.path(), clock).await.unwrap();
    let engine = &opened.engine;
    let admin = Call::with_key(opened.minted.unwrap());
    let event_id = engine
        .emit(&admin, emit())
        .await
        .unwrap()
        .event_id
        .to_string();

    let mut keys = Vec::new();
    for scope in [Scope::Read, Scope::Write, Scope::Admin] {
        let req = CreateKeyRequest {
            scopes: BTreeSet::from([scope]),
            ..CreateKeyRequest::default()
        };
        let created = engine.create_key(&admin, req).await.unwrap();
        keys.push((scope, created.key.expose().to_owned()));
    }

    for op in ALL {
        let err = call(engine, op, &Call::anonymous(), &event_id)
            .await
            .unwrap_err();
        assert_eq!(
            (err.kind, err.message.as_str()),
            (ErrorKind::Unauthenticated, "authentication required"),
            "{op:?}"
        );
        let malformed = Call::with_key(Secret::new("bad-key".into()));
        let err = call(engine, op, &malformed, &event_id).await.unwrap_err();
        assert_eq!(
            (err.kind, err.message.as_str()),
            (ErrorKind::Unauthenticated, "invalid API key"),
            "{op:?}"
        );
        for (held, key) in &keys {
            let cx = Call::with_key(Secret::new(key.clone()));
            let answer = call(engine, op, &cx, &event_id).await;
            match required_scope(op) {
                Some(needed) if needed != *held => {
                    let err = answer.expect_err("refused");
                    let hint = err.hint.as_deref().unwrap_or_default();
                    assert_eq!(
                        (err.kind, err.message.as_str()),
                        (
                            ErrorKind::PermissionDenied,
                            &*format!("missing scope: {needed}")
                        ),
                        "{op:?} with {held}"
                    );
                    assert!(hint.contains(&format!("scope {needed}")), "{hint}");
                    assert!(!err.kind.retryable());
                }
                _ => assert_eq!(answer, Ok(()), "{op:?} with {held}"),
            }
        }
    }
    opened.engine.close().await;
}
