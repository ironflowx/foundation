//! TEN-1's matrix: every operation against no key, a malformed key, a revoked key and each
//! single-scope key. A key holding the operation's scope gets past admission; any other is
//! refused before the handler runs.

use std::collections::BTreeSet;

use ironflowx_cell::ManualClock;
use ironflowx_core::{ErrorKind, Scope, Secret, Timestamp};
use ironflowx_operations::Engine;
use ironflowx_proto::{
    Call, CreateKeyRequest, EmitBatchRequest, EmitRequest, GetEventRequest,
    ListEnvironmentsRequest, ListEventsRequest, ListKeysRequest, OpError, Operation, Operations,
    RevokeKeyRequest, WhoamiRequest, required_scope,
};

/// Every operation. A new one fails to compile in `call` until it joins this list too.
const ALL: [Operation; 9] = [
    Operation::Emit,
    Operation::EmitBatch,
    Operation::GetEvent,
    Operation::ListEvents,
    Operation::Whoami,
    Operation::ListEnvironments,
    Operation::CreateKey,
    Operation::ListKeys,
    Operation::RevokeKey,
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

/// What the operations act on: an event to read and a key to revoke, again and again.
struct Targets {
    event_id: String,
    key_id: String,
}

/// Calls `op` with `cx`, keeping only whether it failed and how.
async fn call(engine: &Engine, op: Operation, cx: &Call, t: &Targets) -> Result<(), OpError> {
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
                event_id: t.event_id.clone(),
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
        Operation::ListKeys => engine
            .list_keys(cx, ListKeysRequest::default())
            .await
            .map(drop),
        Operation::RevokeKey => {
            let req = RevokeKeyRequest {
                key_id: t.key_id.clone(),
            };
            engine.revoke_key(cx, req).await
        }
    }
}

#[tokio::test]
async fn every_operation_admits_exactly_the_keys_holding_its_scope() {
    let dir = tempfile::tempdir().unwrap();
    let clock = ManualClock::new(Timestamp::from_millis(1_800_000_000_000));
    let opened = Engine::open(dir.path(), clock, None).await.unwrap();
    let engine = &opened.engine;
    let admin = Call::with_key(opened.minted.unwrap());
    let event_id = engine
        .emit(&admin, emit())
        .await
        .unwrap()
        .event_id
        .to_string();
    let make = async |scopes: BTreeSet<Scope>| {
        let req = CreateKeyRequest {
            scopes,
            ..CreateKeyRequest::default()
        };
        engine.create_key(&admin, req).await.unwrap()
    };
    // Revoked, again, by every revoke below: revoking a revoked key succeeds.
    let victim = make(BTreeSet::from([Scope::Read])).await;
    let targets = Targets {
        event_id,
        key_id: victim.info.key_id.to_string(),
    };
    let every = [Scope::Read, Scope::Write, Scope::Admin, Scope::Execute];
    let revoked = make(BTreeSet::from(every)).await;
    let revoked_key = revoked.key.expose().to_owned();
    let req = RevokeKeyRequest {
        key_id: revoked.info.key_id.to_string(),
    };
    engine.revoke_key(&admin, req).await.unwrap();

    let mut keys = Vec::new();
    for scope in every {
        let req = CreateKeyRequest {
            scopes: BTreeSet::from([scope]),
            ..CreateKeyRequest::default()
        };
        let created = engine.create_key(&admin, req).await.unwrap();
        keys.push((scope, created.key.expose().to_owned()));
    }

    for op in ALL {
        let err = call(engine, op, &Call::anonymous(), &targets)
            .await
            .unwrap_err();
        assert_eq!(
            (err.kind, err.message.as_str()),
            (ErrorKind::Unauthenticated, "authentication required"),
            "{op:?}"
        );
        let revoked = Call::with_key(Secret::new(revoked_key.clone()));
        let err = call(engine, op, &revoked, &targets).await.unwrap_err();
        assert_eq!(
            (err.kind, err.message.as_str(), &err.hint),
            (ErrorKind::Unauthenticated, "invalid API key", &None),
            "{op:?} with a revoked key"
        );
        let malformed = Call::with_key(Secret::new("bad-key".into()));
        let err = call(engine, op, &malformed, &targets).await.unwrap_err();
        assert_eq!(
            (err.kind, err.message.as_str()),
            (ErrorKind::Unauthenticated, "invalid API key"),
            "{op:?}"
        );
        for (held, key) in &keys {
            let cx = Call::with_key(Secret::new(key.clone()));
            let answer = call(engine, op, &cx, &targets).await;
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

/// A key may hold `execute` alone; it says so, and no other scope implies it or it another (D107).
#[tokio::test]
async fn a_key_holds_execute_alone() {
    let dir = tempfile::tempdir().unwrap();
    let clock = ManualClock::new(Timestamp::from_millis(1_800_000_000_000));
    let opened = Engine::open(dir.path(), clock, None).await.unwrap();
    let engine = &opened.engine;
    let admin = Call::with_key(opened.minted.unwrap());
    let me = engine.whoami(&admin, WhoamiRequest {}).await.unwrap();
    let every = [Scope::Read, Scope::Write, Scope::Admin, Scope::Execute];
    assert_eq!(me.scopes, BTreeSet::from(every), "a new engine's admin key");

    let only = BTreeSet::from([Scope::Execute]);
    let req = CreateKeyRequest {
        scopes: only.clone(),
        ..CreateKeyRequest::default()
    };
    let created = engine.create_key(&admin, req).await.unwrap();
    assert_eq!(created.info.scopes, only);
    let executor = Call::with_key(created.key);
    let me = engine.whoami(&executor, WhoamiRequest {}).await.unwrap();
    assert_eq!(me.scopes, only);
    let listed = engine.list_keys(&admin, ListKeysRequest::default()).await;
    let row = listed
        .unwrap()
        .keys
        .into_iter()
        .find(|k| k.key_id == me.key_id);
    assert_eq!(row.expect("listed").scopes, only);

    // A key without `execute` cannot give it, even an admin one.
    let req = CreateKeyRequest {
        scopes: BTreeSet::from([Scope::Admin, Scope::Read, Scope::Write]),
        ..CreateKeyRequest::default()
    };
    let three = engine.create_key(&admin, req).await.unwrap();
    let req = CreateKeyRequest {
        scopes: only,
        ..CreateKeyRequest::default()
    };
    let err = engine.create_key(&Call::with_key(three.key), req).await;
    assert_eq!(err.unwrap_err().kind, ErrorKind::Invalid);
    opened.engine.close().await;
}
