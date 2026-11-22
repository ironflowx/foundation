//! The control plane through its public interface, on a temporary directory.

use std::collections::BTreeSet;
use std::path::Path;

use ironflowx_authz::{KeyParts, mint};
use ironflowx_core::{Clock, EnvRef, KeyId, KeyOrigin, ProjectId, Scope, TenantId, Timestamp};
use ironflowx_store::{Bootstrap, BootstrapState, ControlPlane, KeyChangeError, NewKey};

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

const HOUR: i64 = 3_600_000;

fn new_key(n: u8, scopes: &[Scope], expires_at: Option<i64>, label: Option<&str>) -> NewKey {
    let id: KeyId = uuid(n).parse().unwrap();
    NewKey {
        id,
        digest: mint(id, [n; 32]).1,
        scopes: scopes.iter().copied().collect(),
        label: label.map(str::to_owned),
        expires_at: expires_at.map(Timestamp::from_millis),
    }
}

#[test]
fn a_created_key_is_stored_with_its_scopes_expiry_and_label() {
    let dir = tempfile::tempdir().unwrap();
    let cp = ControlPlane::open(&path(dir.path())).unwrap();
    let (b, admin) = bootstrap(env(1), 0);
    cp.ensure_bootstrap(&b, &Fixed(T)).unwrap();
    let new = new_key(40, &[Scope::Read], Some(T + HOUR), Some("agent-7"));
    let id = new.id;
    let row = cp
        .insert_key(&env(1), new, admin.id, &Fixed(T + 5))
        .unwrap();
    assert_eq!(row.label.as_deref(), Some("agent-7"));
    assert_eq!(row.created_at, Timestamp::from_millis(T + 5));
    assert_eq!(row.seq, 2, "after the bootstrap key");
    let found = cp.lookup_key(id, &env(1)).unwrap().expect("the new key");
    assert_eq!(found.scopes, BTreeSet::from([Scope::Read]));
    assert_eq!(found.expires_at, Some(Timestamp::from_millis(T + HOUR)));
    assert!(found.digest.ct_eq(&mint(id, [40; 32]).1));
}

/// The creator is read again, and each rule checked, inside the transaction at the clock's now:
/// a creator expired by then, or a request beyond it, changes nothing (TEN-5).
#[test]
fn the_creator_is_checked_inside_the_transaction() {
    let dir = tempfile::tempdir().unwrap();
    let cp = ControlPlane::open(&path(dir.path())).unwrap();
    let (b, admin) = bootstrap(env(1), 0);
    cp.ensure_bootstrap(&b, &Fixed(T)).unwrap();
    let creator = new_key(41, &[Scope::Read, Scope::Admin], Some(T + HOUR), None);
    let creator_id = creator.id;
    cp.insert_key(&env(1), creator, admin.id, &Fixed(T))
        .unwrap();

    let cases = [
        (
            "beyond its scopes",
            new_key(42, &[Scope::Write], Some(T + 1), None),
            T,
        ),
        ("outliving it", new_key(43, &[Scope::Read], None, None), T),
        (
            "a bad label",
            new_key(44, &[Scope::Read], Some(T + 1), Some("a\nb")),
            T,
        ),
        (
            "by an expired creator",
            new_key(45, &[Scope::Read], Some(T + 2 * HOUR), None),
            T + HOUR,
        ),
        (
            "by an unknown creator",
            new_key(46, &[Scope::Read], None, None),
            T,
        ),
    ];
    for (case, new, now) in cases {
        let id = new.id;
        let by = if case == "by an unknown creator" {
            uuid(47).parse().unwrap()
        } else {
            creator_id
        };
        let refused = cp.insert_key(&env(1), new, by, &Fixed(now));
        let creator_refused = case.starts_with("by an");
        assert!(
            matches!(
                (&refused, creator_refused),
                (Err(KeyChangeError::Caller), true) | (Err(KeyChangeError::Refused(_)), false)
            ),
            "{case}: {refused:?}"
        );
        assert!(
            cp.lookup_key(id, &env(1)).unwrap().is_none(),
            "{case}: stored"
        );
    }
    let fine = new_key(48, &[Scope::Read], Some(T + HOUR), None);
    cp.insert_key(&env(1), fine, creator_id, &Fixed(T + HOUR - 1))
        .expect("within the creator");
}

/// Keys list newest first by `seq`, so keys made in the same millisecond keep their order;
/// `before_seq` and `limit` page them (D35).
#[test]
fn keys_list_newest_first_even_within_one_millisecond() {
    let dir = tempfile::tempdir().unwrap();
    let cp = ControlPlane::open(&path(dir.path())).unwrap();
    let (b, admin) = bootstrap(env(1), 0);
    cp.ensure_bootstrap(&b, &Fixed(T)).unwrap();
    let mut made = vec![admin.id];
    for n in 50..53 {
        let new = new_key(n, &[Scope::Read], None, Some("same ms"));
        made.push(new.id);
        cp.insert_key(&env(1), new, admin.id, &Fixed(T)).unwrap();
    }
    made.reverse();
    let all = cp.list_keys(&env(1), None, 10).unwrap();
    let ids: Vec<KeyId> = all.iter().map(|r| r.record.id).collect();
    assert_eq!(ids, made);
    let bootstrap = all.last().unwrap();
    assert_eq!(
        (bootstrap.origin, bootstrap.created_by),
        (KeyOrigin::Minted, None)
    );
    assert_eq!(
        (all[0].origin, all[0].created_by),
        (KeyOrigin::Created, Some(admin.id))
    );
    assert_eq!(all[0].label.as_deref(), Some("same ms"));

    let first = cp.list_keys(&env(1), None, 2).unwrap();
    assert_eq!(
        first.iter().map(|r| r.record.id).collect::<Vec<_>>(),
        made[..2]
    );
    let rest = cp.list_keys(&env(1), Some(first[1].seq), 10).unwrap();
    assert_eq!(
        rest.iter().map(|r| r.record.id).collect::<Vec<_>>(),
        made[2..]
    );
    assert!(
        cp.list_keys(&env(2), None, 10).unwrap().is_empty(),
        "another environment"
    );
}

/// A revoke sets `revoked_at` once and keeps the row; the revoker is read again and checked
/// inside the transaction; a key of another environment or incarnation is not found (D35, D43).
#[test]
fn a_revoke_keeps_the_row_and_checks_the_revoker_inside_the_transaction() {
    let dir = tempfile::tempdir().unwrap();
    let cp = ControlPlane::open(&path(dir.path())).unwrap();
    let (b, admin) = bootstrap(env(1), 0);
    cp.ensure_bootstrap(&b, &Fixed(T)).unwrap();
    let target = new_key(60, &[Scope::Read], None, None);
    let target_id = target.id;
    cp.insert_key(&env(1), target, admin.id, &Fixed(T)).unwrap();
    let revoker = new_key(61, &[Scope::Admin], Some(T + HOUR), None);
    let revoker_id = revoker.id;
    cp.insert_key(&env(1), revoker, admin.id, &Fixed(T))
        .unwrap();

    let revoked_at = |id| cp.lookup_key(id, &env(1)).unwrap().unwrap().revoked_at;
    // An expired revoker changes nothing.
    let late = cp.revoke_key(&env(1), target_id, revoker_id, &Fixed(T + HOUR));
    assert!(matches!(late, Err(KeyChangeError::Caller)), "{late:?}");
    assert_eq!(revoked_at(target_id), None);

    cp.revoke_key(&env(1), target_id, revoker_id, &Fixed(T + 5))
        .unwrap();
    assert_eq!(revoked_at(target_id), Some(Timestamp::from_millis(T + 5)));
    // Again: it succeeds and keeps the first time.
    cp.revoke_key(&env(1), target_id, admin.id, &Fixed(T + 9))
        .unwrap();
    assert_eq!(revoked_at(target_id), Some(Timestamp::from_millis(T + 5)));

    let unknown: KeyId = uuid(62).parse().unwrap();
    let other_token: EnvRef = format!("{}.{:032x}", env(1).id(), 8).parse().unwrap();
    let found = cp.revoke_key(&env(1), unknown, admin.id, &Fixed(T));
    assert!(matches!(found, Err(KeyChangeError::NotFound)), "{found:?}");
    // In another incarnation the revoker is unknown too, so it is refused before the lookup.
    let found = cp.revoke_key(&other_token, target_id, admin.id, &Fixed(T));
    assert!(matches!(found, Err(KeyChangeError::Caller)), "{found:?}");

    // A revoked key changes no key: neither creates nor revokes.
    cp.revoke_key(&env(1), revoker_id, revoker_id, &Fixed(T + 6))
        .unwrap();
    let create = cp.insert_key(
        &env(1),
        new_key(63, &[Scope::Admin], Some(T + 1), None),
        revoker_id,
        &Fixed(T + 7),
    );
    assert!(matches!(create, Err(KeyChangeError::Caller)), "{create:?}");
    let revoke = cp.revoke_key(&env(1), admin.id, revoker_id, &Fixed(T + 7));
    assert!(matches!(revoke, Err(KeyChangeError::Caller)), "{revoke:?}");
    assert_eq!(revoked_at(admin.id), None);
}
