//! TEN-1 and TEN-6 for the embedder's own call (D121): `Engine::embedder_call` is admitted to
//! each operation as admin, with no key, and what it writes is attributed `embedder:<name>`.
//! It reaches only the environment and incarnation of the engine that made it, and a grant of any
//! other type is refused.

use std::collections::BTreeSet;
use std::path::Path;

use ironflowx_cell::ManualClock;
use ironflowx_core::{Emitter, ErrorKind, KeyOrigin, Scope, Timestamp};
use ironflowx_operations::Engine;
use ironflowx_proto::{
    Call, CreateKeyRequest, EmitBatchRequest, EmitRequest, GetEventRequest,
    ListEnvironmentsRequest, ListEventsRequest, ListKeysRequest, OpError, Operation, Operations,
    RevokeKeyRequest, WhoamiRequest,
};

/// The test of each operation, by its function, so a new operation fails to compile here until
/// it has one, and a removed test fails to compile too.
fn tested(op: Operation) -> fn() {
    match op {
        Operation::Emit => emit_is_attributed_to_the_embedder,
        Operation::EmitBatch => a_batch_is_attributed_to_the_embedder,
        Operation::GetEvent => get_event_reads_any_event,
        Operation::ListEvents => list_events_lists_every_event,
        Operation::Whoami => whoami_shows_every_scope_and_no_key,
        Operation::ListEnvironments => list_environments_shows_the_engines,
        Operation::CreateKey => create_key_records_the_embedder_as_creator,
        Operation::ListKeys => list_keys_lists_every_key,
        Operation::RevokeKey => revoke_key_revokes_any_key,
    }
}

#[test]
fn every_operation_has_a_test() {
    // The guard is `tested`'s exhaustive match, at compile time; this only runs it.
    for op in Operation::ALL {
        let _: fn() = tested(*op);
    }
}

fn clock() -> ManualClock {
    ManualClock::new(Timestamp::from_millis(1_800_000_000_000))
}

/// An engine, its bootstrap key's call and an embedder call named `app`.
async fn open(dir: &Path) -> (Engine, Call, Call) {
    let opened = Engine::open(dir, clock(), None).await.unwrap();
    let admin = Call::with_key(opened.minted.unwrap());
    let app = opened.engine.embedder_call("app").unwrap();
    (opened.engine, admin, app)
}

fn app() -> Emitter {
    "embedder:app".parse().unwrap()
}

fn emit(name: &str) -> EmitRequest {
    EmitRequest {
        name: name.into(),
        data: None,
        metadata: None,
        version: None,
        idempotency_key: None,
    }
}

async fn emitted_by(engine: &Engine, cx: &Call, event_id: String) -> Emitter {
    let req = GetEventRequest { event_id };
    engine.get_event(cx, req).await.unwrap().emitted_by
}

const fn first_page() -> ListEventsRequest {
    ListEventsRequest {
        limit: None,
        cursor: None,
    }
}

#[tokio::test]
async fn emit_is_attributed_to_the_embedder() {
    let dir = tempfile::tempdir().unwrap();
    let (engine, admin, app_call) = open(dir.path()).await;
    let sent = engine.emit(&app_call, emit("order.placed")).await.unwrap();
    let by = emitted_by(&engine, &admin, sent.event_id.to_string()).await;
    assert_eq!(by, app());
    assert_eq!(by.to_string(), "embedder:app");
}

#[tokio::test]
async fn a_batch_is_attributed_to_the_embedder() {
    let dir = tempfile::tempdir().unwrap();
    let (engine, admin, app_call) = open(dir.path()).await;
    let req = EmitBatchRequest {
        items: vec![emit("a"), emit("b")],
    };
    let answer = engine.emit_batch(&app_call, req).await.unwrap();
    assert_eq!(answer.items.len(), 2);
    for item in answer.items {
        let id = item.unwrap().event_id.to_string();
        assert_eq!(emitted_by(&engine, &admin, id).await, app());
    }
}

#[tokio::test]
async fn get_event_reads_any_event() {
    let dir = tempfile::tempdir().unwrap();
    let (engine, admin, app_call) = open(dir.path()).await;
    let sent = engine.emit(&admin, emit("order.placed")).await.unwrap();
    let by = emitted_by(&engine, &app_call, sent.event_id.to_string()).await;
    assert_eq!(
        by.to_string(),
        format!("key:{}", key_id(&engine, &admin).await)
    );
}

#[tokio::test]
async fn list_events_lists_every_event() {
    let dir = tempfile::tempdir().unwrap();
    let (engine, admin, app_call) = open(dir.path()).await;
    engine.emit(&admin, emit("a")).await.unwrap();
    engine.emit(&app_call, emit("b")).await.unwrap();
    let page = engine.list_events(&app_call, first_page()).await.unwrap();
    let names: Vec<_> = page.events.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["b", "a"]);
    assert_eq!(page.events[0].emitted_by, app());
}

async fn key_id(engine: &Engine, cx: &Call) -> String {
    let me = engine.whoami(cx, WhoamiRequest {}).await.unwrap();
    me.key_id.expect("a key's call").to_string()
}

#[tokio::test]
async fn whoami_shows_every_scope_and_no_key() {
    let dir = tempfile::tempdir().unwrap();
    let (engine, admin, app_call) = open(dir.path()).await;
    let me = engine.whoami(&app_call, WhoamiRequest {}).await.unwrap();
    let admin_me = engine.whoami(&admin, WhoamiRequest {}).await.unwrap();
    assert_eq!(me.key_id, None);
    // Every scope: what the bootstrap key holds.
    assert_eq!(me.scopes, admin_me.scopes);
    assert_eq!(me.environment, admin_me.environment);
    assert_eq!(me.expires_at, None);
}

#[tokio::test]
async fn list_environments_shows_the_engines() {
    let dir = tempfile::tempdir().unwrap();
    let (engine, admin, app_call) = open(dir.path()).await;
    let mine = engine.list_environments(&app_call, ListEnvironmentsRequest {});
    let admins = engine.list_environments(&admin, ListEnvironmentsRequest {});
    assert_eq!(mine.await.unwrap(), admins.await.unwrap());
}

/// The embedder grants any scope and no expiry, as an admin bootstrap key does; the key names it
/// as creator, through a reopen, and works.
#[tokio::test]
async fn create_key_records_the_embedder_as_creator() {
    let dir = tempfile::tempdir().unwrap();
    let (engine, _, app_call) = open(dir.path()).await;
    let req = CreateKeyRequest {
        scopes: BTreeSet::from([Scope::Read, Scope::Write, Scope::Admin, Scope::Execute]),
        label: Some("worker".into()),
        ..CreateKeyRequest::default()
    };
    let created = engine.create_key(&app_call, req).await.unwrap();
    let info = &created.info;
    assert_eq!(
        (info.origin, info.created_by.clone(), info.expires_at),
        (KeyOrigin::Created, Some(app()), None)
    );
    let made = Call::with_key(created.key);
    let me = engine.whoami(&made, WhoamiRequest {}).await.unwrap();
    assert_eq!(me.key_id, Some(info.key_id));
    let label = CreateKeyRequest {
        label: Some("a\nb".into()),
        ..CreateKeyRequest::default()
    };
    let refused = engine.create_key(&app_call, label).await.unwrap_err();
    let label_rule = "a label is at most 128 bytes, with no control character";
    assert_eq!(
        refused,
        OpError {
            kind: ErrorKind::Invalid,
            message: label_rule.into(),
            hint: None,
        }
    );

    engine.close().await;
    let reopened = Engine::open(dir.path(), clock(), None).await.unwrap();
    let page = reopened.engine.list_keys(&made, ListKeysRequest::default());
    let listed = page.await.unwrap().keys;
    let key = listed.iter().find(|k| k.key_id == info.key_id).unwrap();
    assert_eq!(key.created_by, Some(app()));
}

#[tokio::test]
async fn list_keys_lists_every_key() {
    let dir = tempfile::tempdir().unwrap();
    let (engine, admin, app_call) = open(dir.path()).await;
    let page = engine.list_keys(&app_call, ListKeysRequest::default());
    let keys = page.await.unwrap().keys;
    let ids: Vec<_> = keys.iter().map(|k| k.key_id.to_string()).collect();
    assert_eq!(ids, [key_id(&engine, &admin).await]);
}

#[tokio::test]
async fn revoke_key_revokes_any_key() {
    let dir = tempfile::tempdir().unwrap();
    let (engine, admin, app_call) = open(dir.path()).await;
    let req = RevokeKeyRequest {
        key_id: key_id(&engine, &admin).await,
    };
    engine.revoke_key(&app_call, req).await.unwrap();
    let err = engine.whoami(&admin, WhoamiRequest {}).await.unwrap_err();
    let invalid = OpError {
        kind: ErrorKind::Unauthenticated,
        message: "invalid API key".into(),
        hint: None,
    };
    assert_eq!(err, invalid);
    // The embedder needs no key, so revoking the last one locks it out of nothing.
    engine.emit(&app_call, emit("still.in")).await.unwrap();
}

/// A call made before a restart is admitted after it: it holds the directory's environment and
/// incarnation, not a secret.
#[tokio::test]
async fn an_embedder_call_outlives_its_engine() {
    let dir = tempfile::tempdir().unwrap();
    let (engine, _, app_call) = open(dir.path()).await;
    engine.close().await;
    let reopened = Engine::open(dir.path(), clock(), None).await.unwrap();
    reopened
        .engine
        .emit(&app_call, emit("again"))
        .await
        .unwrap();
}

/// Each refusal of an embedder call, of a grant that is not one, or of a name, as each
/// operation answers it.
#[tokio::test]
async fn another_engines_grant_or_a_foreign_one_is_refused() {
    let (dir, other_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (engine, _, _) = open(dir.path()).await;
    let (_other, _, theirs) = open(other_dir.path()).await;
    for cx in [
        theirs,
        Call::with_grant("embedder:app"),
        Call::with_grant(()),
    ] {
        let invalid = OpError {
            kind: ErrorKind::Unauthenticated,
            message: "invalid grant".into(),
            hint: None,
        };
        let err = engine.whoami(&cx, WhoamiRequest {}).await.unwrap_err();
        assert_eq!(err, invalid);
        let err = engine.emit(&cx, emit("x")).await.unwrap_err();
        assert_eq!(err, invalid);
    }
    for name in ["", "a\nb", "\u{202E}", &"x".repeat(129)] {
        let refused = engine.embedder_call(name).map(drop).unwrap_err();
        assert_eq!(refused.to_string(), "invalid embedder name", "{name:?}");
    }
    assert!(engine.embedder_call(&"x".repeat(128)).is_ok());
}
