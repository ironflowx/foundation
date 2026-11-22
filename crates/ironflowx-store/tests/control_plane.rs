//! The control plane through its public interface, on a temporary directory.

use std::collections::BTreeSet;
use std::path::Path;

use ironflowx_authz::{KeyParts, mint};
use ironflowx_core::{Clock, EnvRef, KeyId, ProjectId, Scope, TenantId, Timestamp};
use ironflowx_store::{Bootstrap, BootstrapState, ControlPlane};

const T: i64 = 1_800_000_000_000;

struct Fixed(i64);

impl Clock for Fixed {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(self.0)
    }
}

fn env(n: u8) -> EnvRef {
    format!("019a9f3e-7c2b-7d41-8a3e-0000000000{n:02x}.{:032x}", 7)
        .parse()
        .unwrap()
}

fn uuid(n: u8) -> String {
    format!("019a9f3e-7c2b-7d41-8a3e-1000000000{n:02x}")
}

/// A bootstrap of `env`; tenant, project and key IDs differ for each `n`.
fn bootstrap(env: EnvRef, n: u8) -> (Bootstrap, KeyParts) {
    let key_id: KeyId = uuid(16 * n + 3).parse().unwrap();
    let (_, digest) = mint(key_id, [9; 32]);
    let key = KeyParts { id: key_id, digest };
    let b = Bootstrap {
        env,
        tenant: uuid(16 * n + 1).parse().unwrap(),
        project: uuid(16 * n + 2).parse().unwrap(),
        key: key.clone(),
    };
    (b, key)
}

fn path(dir: &Path) -> std::path::PathBuf {
    dir.join("control.db")
}

#[test]
fn bootstrap_state_moves_from_no_file_to_marked() {
    let dir = tempfile::tempdir().unwrap();
    let path = path(dir.path());
    assert_eq!(
        ControlPlane::bootstrap_state(&path).unwrap(),
        BootstrapState::NoFile
    );
    assert!(!path.exists(), "reading the state creates nothing");
    let cp = ControlPlane::open(&path).unwrap();
    assert_eq!(
        ControlPlane::bootstrap_state(&path).unwrap(),
        BootstrapState::Unbootstrapped
    );
    let (b, _) = bootstrap(env(1), 0);
    cp.ensure_bootstrap(&b, &Fixed(T)).unwrap();
    assert_eq!(
        ControlPlane::bootstrap_state(&path).unwrap(),
        BootstrapState::Unmarked { env: env(1) }
    );
    cp.mark_ledger_created(&env(1), &Fixed(T + 1)).unwrap();
    drop(cp);
    assert_eq!(
        ControlPlane::bootstrap_state(&path).unwrap(),
        BootstrapState::Marked { env: env(1) }
    );
}

#[test]
fn the_bootstrap_names_each_parent_and_holds_one_admin_key() {
    let dir = tempfile::tempdir().unwrap();
    let cp = ControlPlane::open(&path(dir.path())).unwrap();
    let (b, key) = bootstrap(env(1), 0);
    cp.ensure_bootstrap(&b, &Fixed(T)).unwrap();
    let e = cp.environment(&env(1)).unwrap();
    assert_eq!(e.env, env(1));
    assert_eq!(e.name, "default");
    assert_eq!(e.project_name, "default");
    assert_eq!(e.project_id, uuid(2).parse::<ProjectId>().unwrap());
    assert_eq!(e.tenant_id, uuid(1).parse::<TenantId>().unwrap());
    assert_eq!(e.created_at, Timestamp::from_millis(T));
    let record = cp
        .lookup_key(key.id, &env(1))
        .unwrap()
        .expect("the admin key");
    assert_eq!(record.id, key.id);
    assert_eq!(record.env, env(1));
    assert_eq!(
        record.scopes,
        BTreeSet::from([Scope::Read, Scope::Write, Scope::Admin])
    );
    assert!(record.digest.ct_eq(&key.digest));
}

#[test]
fn a_bootstrapped_file_is_not_bootstrapped_again() {
    let dir = tempfile::tempdir().unwrap();
    let cp = ControlPlane::open(&path(dir.path())).unwrap();
    cp.ensure_bootstrap(&bootstrap(env(1), 0).0, &Fixed(T))
        .unwrap();
    assert!(
        cp.ensure_bootstrap(&bootstrap(env(2), 1).0, &Fixed(T))
            .is_err()
    );
    assert_eq!(
        ControlPlane::bootstrap_state(&path(dir.path())).unwrap(),
        BootstrapState::Unmarked { env: env(1) }
    );
    assert!(cp.environment(&env(2)).is_err());
}

#[test]
fn a_key_is_found_only_under_its_own_environment_and_incarnation() {
    let dir = tempfile::tempdir().unwrap();
    let cp = ControlPlane::open(&path(dir.path())).unwrap();
    let (b, key) = bootstrap(env(1), 0);
    cp.ensure_bootstrap(&b, &Fixed(T)).unwrap();
    let other_token: EnvRef = format!("{}.{:032x}", env(1).id(), 8).parse().unwrap();
    assert!(cp.lookup_key(key.id, &other_token).unwrap().is_none());
    assert!(cp.lookup_key(key.id, &env(2)).unwrap().is_none());
    let unknown: KeyId = uuid(4).parse().unwrap();
    assert!(cp.lookup_key(unknown, &env(1)).unwrap().is_none());
}

#[test]
fn bootstrap_survives_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let (b, key) = bootstrap(env(1), 0);
    {
        let cp = ControlPlane::open(&path(dir.path())).unwrap();
        cp.ensure_bootstrap(&b, &Fixed(T)).unwrap();
        cp.mark_ledger_created(&env(1), &Fixed(T)).unwrap();
    }
    let cp = ControlPlane::open(&path(dir.path())).unwrap();
    assert!(cp.lookup_key(key.id, &env(1)).unwrap().is_some());
}
