//! Engine rows of the Open and Errors tables in architecture.md that no scenario covers yet.

use ironflowx_authz::{KeyParts, mint};
use ironflowx_cell::{ManualClock, new_env_ref};
use ironflowx_core::{ErrorKind, Secret, Timestamp};
use ironflowx_operations::{Engine, EngineError, SeedState};
use ironflowx_proto::{Call, Operations, WhoamiRequest};
use ironflowx_store::{Bootstrap, BootstrapState, ControlPlane};

fn clock() -> ManualClock {
    ManualClock::new(Timestamp::from_millis(1_800_000_000_000))
}

fn uuid(n: u8) -> String {
    format!("019a9f3e-7c2b-7d41-8a3e-1000000000{n:02x}")
}

#[tokio::test]
async fn an_unmarked_bootstrap_is_refused_and_left_as_it_was() {
    let dir = tempfile::tempdir().unwrap();
    let control = dir.path().join("control.db");
    let env = new_env_ref();
    {
        let cp = ControlPlane::open(&control).unwrap();
        let id = uuid(3).parse().unwrap();
        let bootstrap = Bootstrap {
            env,
            tenant: uuid(1).parse().unwrap(),
            project: uuid(2).parse().unwrap(),
            key: KeyParts {
                id,
                digest: mint(id, [5; 32]).1,
            },
            by_seed: false,
        };
        cp.ensure_bootstrap(&bootstrap, &clock()).unwrap();
    }
    let seed = mint(uuid(4).parse().unwrap(), [6; 32]).0;
    for seed in [None, Some(seed)] {
        let err = Engine::open(dir.path(), clock(), seed)
            .await
            .err()
            .expect("refused");
        assert!(
            matches!(&err, EngineError::Io(e) if e.to_string().contains("did not finish the bootstrap")),
            "{err:?}"
        );
        assert_eq!(
            ControlPlane::bootstrap_state(&control).unwrap(),
            BootstrapState::Unmarked { env }
        );
    }
    let keys = ControlPlane::open(&control)
        .unwrap()
        .list_keys(&env, None, 10);
    assert_eq!(keys.unwrap().len(), 1, "the seed was not applied");
    assert!(
        !dir.path()
            .join("ledgers")
            .join(env.id().to_string())
            .exists()
    );
}

/// A `control.db` with no bootstrap in it is bootstrapped, with a seed or without.
#[tokio::test]
async fn a_never_bootstrapped_control_plane_is_bootstrapped() {
    let seed = || mint(uuid(5).parse().unwrap(), [7; 32]).0;
    for given in [None, Some(seed())] {
        let dir = tempfile::tempdir().unwrap();
        drop(ControlPlane::open(&dir.path().join("control.db")).unwrap());
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
