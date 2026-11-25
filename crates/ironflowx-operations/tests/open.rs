//! Engine rows of the Open and Errors tables in architecture.md that no scenario covers yet.

use ironflowx_authz::mint;
use ironflowx_cell::{ManualClock, new_env_ref};
use ironflowx_core::{ErrorKind, Secret, Timestamp};
use ironflowx_operations::{Engine, EngineError, SeedState, StoreError, is_bootstrapped};
use ironflowx_proto::{Call, ListEnvironmentsRequest, Operations, WhoamiRequest};
use ironflowx_store::{Bootstrap, ControlPlane};

fn clock() -> ManualClock {
    ManualClock::new(Timestamp::from_millis(1_800_000_000_000))
}

fn uuid(n: u8) -> String {
    format!("019a9f3e-7c2b-7d41-8a3e-1000000000{n:02x}")
}

#[test]
fn a_missing_control_plane_is_not_bootstrapped() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("missing");
    assert!(!is_bootstrapped(&missing).unwrap());
    assert!(!missing.exists());
    assert!(!is_bootstrapped(dir.path()).unwrap());
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[test]
fn only_a_marked_bootstrap_counts() {
    let dir = tempfile::tempdir().unwrap();
    let control = ControlPlane::open(&dir.path().join("control.db")).unwrap();
    assert!(!is_bootstrapped(dir.path()).unwrap(), "unbootstrapped");

    let id = uuid(5).parse().unwrap();
    let (_, digest) = mint(id, [7; 32]);
    let env = new_env_ref();
    let bootstrap = Bootstrap {
        env,
        tenant: uuid(1).parse().unwrap(),
        project: uuid(2).parse().unwrap(),
        key: ironflowx_authz::KeyParts { id, digest },
        by_seed: false,
    };
    control.ensure_bootstrap(&bootstrap, &clock()).unwrap();
    assert!(!is_bootstrapped(dir.path()).unwrap(), "unmarked");

    control.mark_ledger_created(&env, &clock()).unwrap();
    drop(control);
    assert!(is_bootstrapped(dir.path()).unwrap(), "marked");
}

#[test]
fn a_broken_control_plane_is_a_bootstrap_read_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("control.db");
    std::fs::write(&path, "not a database").unwrap();
    let StoreError::Failed(expected) =
        ControlPlane::bootstrap_state(&path).expect_err("the control plane is corrupt")
    else {
        panic!("corruption should fail the read");
    };
    let error = is_bootstrapped(dir.path()).expect_err("bootstrap read failed");
    let EngineError::Store(StoreError::Failed(message)) = error else {
        panic!("expected the failed store read, got {error:?}");
    };
    assert_eq!(message, expected);
}

/// A `control.db` with no bootstrap in it is bootstrapped, with a seed or without: one with its
/// schema, and one that a crash left empty before its schema was committed.
#[tokio::test]
async fn a_never_bootstrapped_control_plane_is_bootstrapped() {
    type Make = fn(&std::path::Path);
    let with_schema: Make = |path| drop(ControlPlane::open(path).unwrap());
    let empty: Make = |path| std::fs::write(path, "").unwrap();
    let no_table: Make = |path| {
        let db = rusqlite::Connection::open(path).unwrap();
        db.pragma_update(None, "journal_mode", "WAL").unwrap();
    };
    let seed = || mint(uuid(5).parse().unwrap(), [7; 32]).0;
    for (given, make) in [
        (None, with_schema),
        (Some(seed()), with_schema),
        (None, empty),
        (None, no_table),
    ] {
        let dir = tempfile::tempdir().unwrap();
        make(&dir.path().join("control.db"));
        let with_seed = given.is_some();
        let opened = Engine::open(dir.path(), clock(), given).await.unwrap();
        assert_eq!(opened.minted.is_none(), with_seed);
        assert_eq!(opened.seed, with_seed.then_some(SeedState::Current));
        let key = opened.minted.unwrap_or_else(seed);
        let cx = Call::with_key(key);
        let whoami = opened.engine.whoami(&cx, WhoamiRequest {}).await;
        whoami.expect("the bootstrap key");
        opened.engine.close().await;
    }
}

/// The listed environment's reference and project ID are the stored ones; ACC-010 shows only that
/// two keys agree on them.
#[tokio::test]
async fn the_listed_environment_has_the_ids_the_control_plane_stores() {
    let dir = tempfile::tempdir().unwrap();
    let opened = Engine::open(dir.path(), clock(), None).await.unwrap();
    let cx = Call::with_key(opened.minted.unwrap());
    let listed = opened
        .engine
        .list_environments(&cx, ListEnvironmentsRequest {})
        .await
        .unwrap();
    let ids: Vec<_> = listed
        .iter()
        .map(|e| (e.env.to_string(), e.project_id.to_string()))
        .collect();
    let db = rusqlite::Connection::open(dir.path().join("control.db")).unwrap();
    let sql = "SELECT id || '.' || token, project_id FROM environments";
    let mut stmt = db.prepare(sql).unwrap();
    let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    let stored: Vec<(String, String)> = rows.map(Result::unwrap).collect();
    assert_eq!(ids, stored);
    opened.engine.close().await;
}

#[tokio::test]
async fn a_failed_control_plane_read_is_unavailable_and_the_engine_stays_open() {
    let dir = tempfile::tempdir().unwrap();
    let opened = Engine::open(dir.path(), clock(), None).await.unwrap();
    let key = || {
        Call::with_key(Secret::new(
            opened.minted.as_ref().unwrap().expose().to_owned(),
        ))
    };
    let db = rusqlite::Connection::open(dir.path().join("control.db")).unwrap();
    db.execute_batch("ALTER TABLE keys RENAME TO keys_away")
        .unwrap();
    let err = opened
        .engine
        .whoami(&key(), WhoamiRequest {})
        .await
        .expect_err("the key lookup fails");
    assert_eq!(err.kind, ErrorKind::Unavailable, "{err:?}");
    assert!(err.kind.retryable());
    assert_eq!(err.hint.as_deref(), Some("retry at once"));
    db.execute_batch("ALTER TABLE keys_away RENAME TO keys")
        .unwrap();
    opened
        .engine
        .whoami(&key(), WhoamiRequest {})
        .await
        .expect("the same engine answers once the control plane reads again");
    opened.engine.close().await;
}
