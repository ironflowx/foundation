//! A failure at each control-plane seam answers `Unavailable`, retried at once, for a call that
//! reaches it; a call refused before the lookup is unaffected; the engine answers normally once
//! the failure clears (D44). With the ledger `Unavailable`, key and identity operations still
//! work. Each failure is made from another connection, so the engine carries no test hook.

use std::collections::BTreeSet;
use std::path::Path;

use ironflowx_cell::ManualClock;
use ironflowx_core::{ErrorKind, Scope, Secret, Timestamp};
use ironflowx_operations::Engine;
use ironflowx_proto::{
    Call, CreateKeyRequest, EmitRequest, ListEnvironmentsRequest, ListEventsRequest,
    ListKeysRequest, OpError, RevokeKeyRequest, WhoamiRequest,
};

async fn open(dir: &Path) -> (Engine, String) {
    let clock = ManualClock::new(Timestamp::from_millis(1_800_000_000_000));
    let opened = Engine::open(dir, clock, None).await.unwrap();
    (opened.engine, opened.minted.unwrap().expose().to_owned())
}

fn call(key: &str) -> Call {
    Call::with_key(Secret::new(key.to_owned()))
}

fn assert_unavailable<T: std::fmt::Debug>(answer: Result<T, OpError>, seam: &str) {
    let err = answer.expect_err(seam);
    assert_eq!(err.kind, ErrorKind::Unavailable, "{seam}: {err:?}");
    assert!(err.kind.retryable(), "{seam}");
    assert_eq!(err.hint.as_deref(), Some("retry at once"), "{seam}");
}

/// Breaks one seam with `break_sql`, checks the call that reaches it, then mends it.
async fn at_seam(dir: &Path, break_sql: &str, mend_sql: &str, check: impl AsyncFnOnce()) {
    let db = rusqlite::Connection::open(dir.join("control.db")).unwrap();
    db.execute_batch(break_sql).unwrap();
    check().await;
    db.execute_batch(mend_sql).unwrap();
}

#[tokio::test]
async fn a_failure_at_each_control_plane_seam_is_unavailable_until_it_clears() {
    let dir = tempfile::tempdir().unwrap();
    let (engine, admin) = open(dir.path()).await;
    let k = call(&admin);
    let created = engine
        .create_key(&k, CreateKeyRequest::default())
        .await
        .unwrap();
    let victim = RevokeKeyRequest {
        key_id: created.info.key_id.to_string(),
    };

    // The lookup: every call that presents a well-formed key reaches it.
    let lookup = async || {
        assert_unavailable(engine.whoami(&k, WhoamiRequest {}).await, "lookup");
        let refused = engine.whoami(&Call::anonymous(), WhoamiRequest {}).await;
        assert_eq!(refused.unwrap_err().message, "authentication required");
        let refused = engine.whoami(&call("bad-key"), WhoamiRequest {}).await;
        assert_eq!(refused.unwrap_err().message, "invalid API key");
    };
    at_seam(
        dir.path(),
        "ALTER TABLE keys RENAME TO keys_gone",
        "ALTER TABLE keys_gone RENAME TO keys",
        lookup,
    )
    .await;

    // The environment read, after admission.
    let environment = async || {
        assert_unavailable(engine.whoami(&k, WhoamiRequest {}).await, "environment");
        let listed = engine
            .list_environments(&k, ListEnvironmentsRequest {})
            .await;
        assert_unavailable(listed, "environment");
    };
    at_seam(
        dir.path(),
        "ALTER TABLE environments RENAME TO environments_gone",
        "ALTER TABLE environments_gone RENAME TO environments",
        environment,
    )
    .await;

    // The listing, which reads columns admission does not.
    let listing = async || {
        let listed = engine.list_keys(&k, ListKeysRequest::default()).await;
        assert_unavailable(listed, "listing");
    };
    at_seam(
        dir.path(),
        "ALTER TABLE keys RENAME COLUMN label TO label_gone",
        "ALTER TABLE keys RENAME COLUMN label_gone TO label",
        listing,
    )
    .await;

    // A key change: both kinds write to the keys table.
    let change = async || {
        let made = engine.create_key(&k, CreateKeyRequest::default()).await;
        assert_unavailable(made, "key change");
        assert_unavailable(engine.revoke_key(&k, victim.clone()).await, "key change");
    };
    at_seam(
        dir.path(),
        "CREATE TRIGGER no_insert BEFORE INSERT ON keys BEGIN SELECT RAISE(ABORT, 'x'); END;
         CREATE TRIGGER no_update BEFORE UPDATE ON keys BEGIN SELECT RAISE(ABORT, 'x'); END;",
        "DROP TRIGGER no_insert; DROP TRIGGER no_update;",
        change,
    )
    .await;

    // Cleared: every seam answers again, and the engine never closed.
    engine.whoami(&k, WhoamiRequest {}).await.unwrap();
    let page = engine
        .list_keys(&k, ListKeysRequest::default())
        .await
        .unwrap();
    assert_eq!(
        page.keys.len(),
        2,
        "nothing was written while the change failed"
    );
    engine.revoke_key(&k, victim).await.unwrap();
    engine.close().await;
}

/// Key and identity operations do not touch the ledger, so they keep working while it is
/// `Unavailable`; an admin can still inspect and revoke keys.
#[tokio::test]
async fn key_operations_work_while_the_ledger_is_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    let (engine, admin) = open(dir.path()).await;
    let k = call(&admin);
    let env = engine
        .list_environments(&k, ListEnvironmentsRequest {})
        .await
        .unwrap()
        .remove(0)
        .env;
    let read_db = dir
        .path()
        .join("ledgers")
        .join(env.id().to_string())
        .join("read.db");
    // A read that fails in the read store fails the ledger until it is reopened.
    let db = rusqlite::Connection::open(read_db).unwrap();
    db.execute_batch("DROP TABLE events").unwrap();
    let page = ListEventsRequest {
        limit: None,
        cursor: None,
    };
    assert_eq!(
        engine.list_events(&k, page).await.unwrap_err().kind,
        ErrorKind::Unavailable
    );
    let emit = EmitRequest {
        name: "order.placed".into(),
        data: None,
        metadata: None,
        version: None,
        idempotency_key: None,
    };
    assert_eq!(
        engine.emit(&k, emit).await.unwrap_err().kind,
        ErrorKind::Unavailable
    );

    engine.whoami(&k, WhoamiRequest {}).await.expect("whoami");
    let req = CreateKeyRequest {
        scopes: BTreeSet::from([Scope::Read]),
        ..CreateKeyRequest::default()
    };
    let created = engine.create_key(&k, req).await.expect("create_key");
    let listed = engine.list_keys(&k, ListKeysRequest::default()).await;
    assert_eq!(listed.expect("list_keys").keys.len(), 2);
    let req = RevokeKeyRequest {
        key_id: created.info.key_id.to_string(),
    };
    engine.revoke_key(&k, req).await.expect("revoke_key");
    engine.close().await;
}
