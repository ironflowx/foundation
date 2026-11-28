//! The control plane through its public interface, on a temporary directory.

mod writes;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::mpsc::{Sender, channel};
use std::time::{Duration, Instant};

use ironflowx_authz::{KeyParts, KeyRecord, SeedState, mint};
use ironflowx_core::{
    Clock, Emitter, EnvRef, KeyId, KeyOrigin, ProjectId, Scope, TenantId, Timestamp,
};
use rusqlite::types::Value;

use ironflowx_store::{
    Bootstrap, BootstrapState, ControlPlane, KeyChangeError, NewKey, RecoverError, SeedError,
    StoreError,
};

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
        by_seed: false,
    };
    (b, key)
}

fn path(dir: &Path) -> std::path::PathBuf {
    dir.join("control.db")
}

/// The stored key, as admission looks it up.
fn found(cp: &ControlPlane, id: KeyId, env: &EnvRef) -> Option<KeyRecord> {
    cp.lookup_key(id, env, &Fixed(T)).unwrap().0
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
    let record = found(&cp, key.id, &env(1)).expect("the admin key");
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
    assert!(found(&cp, key.id, &other_token).is_none());
    assert!(found(&cp, key.id, &env(2)).is_none());
    let unknown: KeyId = uuid(4).parse().unwrap();
    assert!(found(&cp, unknown, &env(1)).is_none());
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
    assert!(found(&cp, key.id, &env(1)).is_some());
}

const HOUR: i64 = 3_600_000;
/// Room for every key a test creates, unless it tests the cap.
const CAP: u32 = 1_000;

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
        .insert_key(&env(1), new, &Emitter::from(admin.id), &Fixed(T + 5), CAP)
        .unwrap();
    assert_eq!(row.label.as_deref(), Some("agent-7"));
    assert_eq!(row.created_at, Timestamp::from_millis(T + 5));
    assert_eq!(row.seq, 2, "after the bootstrap key");
    let found = found(&cp, id, &env(1)).expect("the new key");
    assert_eq!(found.scopes, BTreeSet::from([Scope::Read]));
    assert_eq!(found.expires_at, Some(Timestamp::from_millis(T + HOUR)));
    assert!(found.digest.ct_eq(&mint(id, [40; 32]).1));
}

/// The embedding process creates and revokes with no record to read again, as a creator with
/// every scope and no expiry, and is recorded as its `embedder:<name>`, read back after a reopen;
/// a creating key is recorded by its bare ID, as since 0.2, however it is named; a stored creator
/// that is neither a key ID nor an embedder fails the read (D121).
#[test]
fn an_embedder_changes_keys_with_no_record() {
    let dir = tempfile::tempdir().unwrap();
    let cp = ControlPlane::open(&path(dir.path())).unwrap();
    let (b, admin) = bootstrap(env(1), 0);
    cp.ensure_bootstrap(&b, &Fixed(T)).unwrap();
    let app: Emitter = "embedder:app".parse().unwrap();
    let by = app.clone();
    let new = new_key(40, &[Scope::Read, Scope::Write, Scope::Admin], None, None);
    let id = new.id;
    let row = cp.insert_key(&env(1), new, &by, &Fixed(T), CAP).unwrap();
    assert_eq!(row.created_by.as_ref(), Some(&app));
    let label = new_key(41, &[Scope::Read], None, Some("a\nb"));
    let refused = cp.insert_key(&env(1), label, &by, &Fixed(T), CAP);
    assert!(
        matches!(refused, Err(KeyChangeError::Refused(_))),
        "{refused:?}"
    );
    cp.revoke_key(&env(1), id, &by, &Fixed(T + 1)).unwrap();
    // An embedder of no current environment and incarnation changes nothing, as a key of one.
    let stale: EnvRef = format!("{}.{:032x}", env(1).id(), 8).parse().unwrap();
    let elsewhere = new_key(43, &[Scope::Read], None, None);
    let refused = cp.insert_key(&stale, elsewhere, &by, &Fixed(T), CAP);
    assert!(
        matches!(refused, Err(KeyChangeError::Caller)),
        "{refused:?}"
    );
    // Nor of another environment ID with this incarnation's token.
    let other = new_key(44, &[Scope::Read], None, None);
    let refused = cp.insert_key(&env(2), other, &by, &Fixed(T), CAP);
    assert!(
        matches!(refused, Err(KeyChangeError::Caller)),
        "{refused:?}"
    );
    let admin_by: Emitter = format!("key:{}", admin.id).parse().unwrap();
    let made = new_key(42, &[Scope::Read], None, None);
    let made_id = made.id;
    cp.insert_key(&env(1), made, &admin_by, &Fixed(T), CAP)
        .unwrap();
    drop(cp);
    let cp = ControlPlane::open(&path(dir.path())).unwrap();
    let rows = cp.list_keys(&env(1), None, 10).unwrap();
    assert_eq!(
        (rows[0].record.id, rows[0].created_by.as_ref()),
        (made_id, Some(&admin_by))
    );
    assert_eq!(
        (rows[1].record.id, rows[1].created_by.as_ref()),
        (id, Some(&app))
    );
    assert_eq!(
        rows[1].record.revoked_at,
        Some(Timestamp::from_millis(T + 1))
    );
    let db = rusqlite::Connection::open(path(dir.path())).unwrap();
    for bad in [
        "embedder:",
        "key:0190b5f0-0000-7000-8000-000000000001",
        "app",
    ] {
        db.execute(
            "UPDATE keys SET created_by = ?1 WHERE id = ?2",
            [bad, &id.to_string()],
        )
        .unwrap();
        assert!(cp.list_keys(&env(1), None, 10).is_err(), "{bad}");
    }
}

/// The creator is read again, and each rule checked, inside the transaction at the clock's now:
/// a creator expired by then, or a request beyond it, creates no key (TEN-5).
#[test]
fn the_creator_is_checked_inside_the_transaction() {
    let dir = tempfile::tempdir().unwrap();
    let cp = ControlPlane::open(&path(dir.path())).unwrap();
    let (b, admin) = bootstrap(env(1), 0);
    cp.ensure_bootstrap(&b, &Fixed(T)).unwrap();
    let creator = new_key(41, &[Scope::Read, Scope::Admin], Some(T + HOUR), None);
    let creator_id = creator.id;
    cp.insert_key(&env(1), creator, &Emitter::from(admin.id), &Fixed(T), CAP)
        .unwrap();

    let expiring = new_key(49, &[Scope::Read, Scope::Admin], Some(T + HOUR), None);
    let expiring_id = expiring.id;
    cp.insert_key(&env(1), expiring, &Emitter::from(admin.id), &Fixed(T), CAP)
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
        let by = match case {
            "by an unknown creator" => uuid(47).parse().unwrap(),
            "by an expired creator" => expiring_id,
            _ => creator_id,
        };
        let refused = cp.insert_key(&env(1), new, &Emitter::from(by), &Fixed(now), CAP);
        let creator_refused = case.starts_with("by an");
        assert!(
            matches!(
                (&refused, creator_refused),
                (Err(KeyChangeError::Caller), true) | (Err(KeyChangeError::Refused(_)), false)
            ),
            "{case}: {refused:?}"
        );
        assert!(found(&cp, id, &env(1)).is_none(), "{case}: stored");
    }
    let fine = new_key(48, &[Scope::Read], Some(T + HOUR), None);
    cp.insert_key(
        &env(1),
        fine,
        &Emitter::from(creator_id),
        &Fixed(T + HOUR - 1),
        CAP,
    )
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
        cp.insert_key(&env(1), new, &Emitter::from(admin.id), &Fixed(T), CAP)
            .unwrap();
    }
    made.reverse();
    let all = cp.list_keys(&env(1), None, 10).unwrap();
    let ids: Vec<KeyId> = all.iter().map(|r| r.record.id).collect();
    assert_eq!(ids, made);
    let bootstrap = all.last().unwrap();
    assert_eq!(
        (bootstrap.origin, bootstrap.created_by.clone()),
        (KeyOrigin::Minted, None)
    );
    assert_eq!(
        (all[0].origin, all[0].created_by.clone()),
        (KeyOrigin::Created, Some(admin.id.into()))
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
    cp.insert_key(&env(1), target, &Emitter::from(admin.id), &Fixed(T), CAP)
        .unwrap();
    let revoker = new_key(61, &[Scope::Admin], Some(T + HOUR), None);
    let revoker_id = revoker.id;
    cp.insert_key(&env(1), revoker, &Emitter::from(admin.id), &Fixed(T), CAP)
        .unwrap();

    let revoked_at = |id| found(&cp, id, &env(1)).unwrap().revoked_at;
    // An expired revoker revokes nothing.
    let expiring = new_key(64, &[Scope::Admin], Some(T + HOUR), None);
    let expiring_id = expiring.id;
    cp.insert_key(&env(1), expiring, &Emitter::from(admin.id), &Fixed(T), CAP)
        .unwrap();
    let late = cp.revoke_key(
        &env(1),
        target_id,
        &Emitter::from(expiring_id),
        &Fixed(T + HOUR),
    );
    assert!(matches!(late, Err(KeyChangeError::Caller)), "{late:?}");
    assert_eq!(revoked_at(target_id), None);

    cp.revoke_key(
        &env(1),
        target_id,
        &Emitter::from(revoker_id),
        &Fixed(T + 5),
    )
    .unwrap();
    assert_eq!(revoked_at(target_id), Some(Timestamp::from_millis(T + 5)));
    // Again: it succeeds and keeps the first time.
    cp.revoke_key(&env(1), target_id, &Emitter::from(admin.id), &Fixed(T + 9))
        .unwrap();
    assert_eq!(revoked_at(target_id), Some(Timestamp::from_millis(T + 5)));

    let unknown: KeyId = uuid(62).parse().unwrap();
    let other_token: EnvRef = format!("{}.{:032x}", env(1).id(), 8).parse().unwrap();
    let found = cp.revoke_key(&env(1), unknown, &Emitter::from(admin.id), &Fixed(T));
    assert!(matches!(found, Err(KeyChangeError::NotFound)), "{found:?}");
    // In another incarnation the revoker is unknown too, so it is refused before the lookup.
    let found = cp.revoke_key(&other_token, target_id, &Emitter::from(admin.id), &Fixed(T));
    assert!(matches!(found, Err(KeyChangeError::Caller)), "{found:?}");

    // A revoked key changes no key: neither creates nor revokes.
    cp.revoke_key(
        &env(1),
        revoker_id,
        &Emitter::from(revoker_id),
        &Fixed(T + 6),
    )
    .unwrap();
    let create = cp.insert_key(
        &env(1),
        new_key(63, &[Scope::Admin], Some(T + 1), None),
        &Emitter::from(revoker_id),
        &Fixed(T + 7),
        CAP,
    );
    assert!(matches!(create, Err(KeyChangeError::Caller)), "{create:?}");
    let revoke = cp.revoke_key(&env(1), admin.id, &Emitter::from(revoker_id), &Fixed(T + 7));
    assert!(matches!(revoke, Err(KeyChangeError::Caller)), "{revoke:?}");
    assert_eq!(revoked_at(admin.id), None);
}

/// Every key's recorded expiry and revoke, by ID.
fn marks(cp: &ControlPlane) -> BTreeMap<String, (bool, Option<Timestamp>)> {
    let rows = cp.list_keys(&env(1), None, 2_000).unwrap();
    rows.into_iter()
        .map(|r| {
            (
                r.record.id.to_string(),
                (r.record.expiry_recorded, r.record.revoked_at),
            )
        })
        .collect()
}

/// Recording an expiry sets one key's flag, in its own environment and incarnation only, and
/// keeps it across a reopen; a revoked key keeps its row as it is (D42, TEN-13).
#[test]
fn recording_an_expiry_marks_that_key_only_and_survives_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let cp = ControlPlane::open(&path(dir.path())).unwrap();
    let (b, admin) = bootstrap(env(1), 0);
    cp.ensure_bootstrap(&b, &Fixed(T)).unwrap();
    let mut ids = Vec::new();
    for n in 70..73 {
        let new = new_key(n, &[Scope::Read], Some(T + HOUR), None);
        ids.push(new.id);
        cp.insert_key(&env(1), new, &Emitter::from(admin.id), &Fixed(T), CAP)
            .unwrap();
    }
    cp.revoke_key(&env(1), ids[1], &Emitter::from(admin.id), &Fixed(T))
        .unwrap();
    let mut expected = BTreeMap::from([
        (admin.id.to_string(), (false, None)),
        (ids[0].to_string(), (false, None)),
        (ids[1].to_string(), (false, Some(Timestamp::from_millis(T)))),
        (ids[2].to_string(), (false, None)),
    ]);
    assert_eq!(marks(&cp), expected, "before");
    let db = rusqlite::Connection::open(path(dir.path())).unwrap();
    db.execute_batch(
        "CREATE TABLE writes (id TEXT);
         CREATE TRIGGER count AFTER UPDATE ON keys BEGIN INSERT INTO writes VALUES (new.id); END;",
    )
    .unwrap();
    let writes = || -> i64 {
        db.query_row("SELECT count(*) FROM writes", [], |r| r.get(0))
            .unwrap()
    };

    let other_token: EnvRef = format!("{}.{:032x}", env(1).id(), 8).parse().unwrap();
    cp.record_expiry(&other_token, ids[0]).unwrap();
    cp.record_expiry(&env(2), ids[0]).unwrap();
    cp.record_expiry(&env(1), ids[1]).unwrap();
    assert_eq!(marks(&cp), expected, "foreign or revoked");
    assert_eq!(writes(), 0, "foreign or revoked");

    cp.record_expiry(&env(1), ids[0]).unwrap();
    cp.record_expiry(&env(1), ids[0]).expect("again");
    assert_eq!(writes(), 1, "once per key (D42)");
    expected.insert(ids[0].to_string(), (true, None));
    assert_eq!(marks(&cp), expected);
    drop(cp);
    let cp = ControlPlane::open(&path(dir.path())).unwrap();
    assert_eq!(marks(&cp), expected, "after a reopen");
    let found = found(&cp, ids[0], &env(1)).unwrap();
    assert!(found.expiry_recorded, "as admission reads it");
}

/// A key change whose caller is newly expired records that caller's expiry in its own
/// transaction and changes nothing else; a caller that is revoked as well records nothing.
#[test]
fn a_change_by_a_newly_expired_caller_records_only_its_expiry() {
    let dir = tempfile::tempdir().unwrap();
    let cp = ControlPlane::open(&path(dir.path())).unwrap();
    let (b, admin) = bootstrap(env(1), 0);
    cp.ensure_bootstrap(&b, &Fixed(T)).unwrap();
    let mut callers = Vec::new();
    for n in 80..84 {
        let new = new_key(n, &[Scope::Admin], Some(T + HOUR), None);
        callers.push(new.id);
        cp.insert_key(&env(1), new, &Emitter::from(admin.id), &Fixed(T), CAP)
            .unwrap();
    }
    for id in &callers[2..] {
        cp.revoke_key(&env(1), *id, &Emitter::from(admin.id), &Fixed(T))
            .unwrap();
    }
    let late = Fixed(T + HOUR);
    for (i, by) in callers.iter().copied().enumerate() {
        let mut expected = marks(&cp);
        expected.get_mut(&by.to_string()).unwrap().0 = i < 2;
        let refused = if i % 2 == 0 {
            let new = new_key(90, &[Scope::Admin], Some(T + 2 * HOUR), None);
            cp.insert_key(&env(1), new, &Emitter::from(by), &late, CAP)
                .map(drop)
        } else {
            cp.revoke_key(&env(1), admin.id, &Emitter::from(by), &late)
        };
        assert!(
            matches!(refused, Err(KeyChangeError::Caller)),
            "{i}: {refused:?}"
        );
        assert_eq!(marks(&cp), expected, "{i}");
    }
    // Recorded, so the clock set back does not bring it back.
    let back = cp.revoke_key(&env(1), admin.id, &Emitter::from(callers[0]), &Fixed(T));
    assert!(matches!(back, Err(KeyChangeError::Caller)), "{back:?}");
}

/// A created key holds a slot until it is revoked or its expiry is recorded, whatever the clock
/// reads; keys of any other origin, and of another incarnation, hold none (TEN-15).
#[test]
fn a_slot_is_held_until_a_revoke_or_a_recorded_expiry() {
    let dir = tempfile::tempdir().unwrap();
    let cp = ControlPlane::open(&path(dir.path())).unwrap();
    let (b, admin) = bootstrap(env(1), 0);
    cp.ensure_bootstrap(&b, &Fixed(T)).unwrap();
    let mut n = 100;
    let mut create = |now: i64| {
        n += 1;
        let new = new_key(n, &[Scope::Read], Some(now + HOUR), None);
        let id = new.id;
        cp.insert_key(&env(1), new, &Emitter::from(admin.id), &Fixed(now), 3)
            .map(|_| id)
    };
    let full = |r: Result<KeyId, KeyChangeError>, case: &str| {
        assert!(matches!(r, Err(KeyChangeError::Full)), "{case}: {r:?}");
    };
    let a = create(T).unwrap();
    let b = create(T).unwrap();
    create(T).expect("the cap itself");
    full(create(T), "the cap, plus one");
    full(create(T + HOUR), "expired by the clock only");
    // A request that breaks a rule is refused for that, cap or no cap.
    let bad = new_key(99, &[], None, None);
    let bad = cp.insert_key(&env(1), bad, &Emitter::from(admin.id), &Fixed(T), 3);
    assert!(matches!(bad, Err(KeyChangeError::Refused(_))), "{bad:?}");

    cp.revoke_key(&env(1), a, &Emitter::from(admin.id), &Fixed(T))
        .unwrap();
    create(T).expect("after a revoke");
    full(create(T), "one slot freed, one taken");
    cp.record_expiry(&env(1), b).unwrap();
    let last = create(T).expect("after a recorded expiry");
    full(create(T), "one slot freed, one taken");
    cp.revoke_key(&env(1), b, &Emitter::from(admin.id), &Fixed(T))
        .unwrap();
    full(create(T), "a recorded expiry's slot is freed once");

    // The other origins, through the table.
    let db = rusqlite::Connection::open(path(dir.path())).unwrap();
    let set = |column: &str, value: &str| {
        let sql = format!("UPDATE keys SET {column} = ?1 WHERE id = ?2");
        assert_eq!(db.execute(&sql, [value, &last.to_string()]).unwrap(), 1);
    };
    for origin in ["seed", "recovered", "minted"] {
        set("origin", origin);
        let made = create(T).expect(origin);
        full(create(T), origin);
        cp.revoke_key(&env(1), made, &Emitter::from(admin.id), &Fixed(T))
            .unwrap();
        set("origin", "created");
        full(create(T), "a created key again");
    }
    // A key of another environment, which this file does not hold, or of another incarnation.
    db.pragma_update(None, "foreign_keys", false).unwrap();
    let (id, token) = (env(1).id().to_string(), env(1).token().to_string());
    for (column, own, other) in [
        ("environment_token", token, format!("{:032x}", 8)),
        ("environment_id", id, env(2).id().to_string()),
    ] {
        set(column, &other);
        let made = create(T).expect(column);
        full(create(T), column);
        cp.revoke_key(&env(1), made, &Emitter::from(admin.id), &Fixed(T))
            .unwrap();
        set(column, &own);
        full(create(T), "a key of this environment again");
    }
}

/// A seed: a key the host made, its ID from `n` and its secret from `secret`.
fn seed(n: u8, secret: u8) -> KeyParts {
    let id: KeyId = uuid(n).parse().unwrap();
    KeyParts {
        id,
        digest: mint(id, [secret; 32]).1,
    }
}

/// Every key row by ID: each column's value by its name.
fn whole_rows(dir: &Path) -> BTreeMap<String, BTreeMap<String, Value>> {
    let db = rusqlite::Connection::open(path(dir)).unwrap();
    let mut stmt = db.prepare("SELECT * FROM keys").unwrap();
    let names: Vec<String> = stmt.column_names().map(str::to_owned).collect();
    let rows = stmt.query_map([], |r| {
        let row: Result<BTreeMap<_, _>, rusqlite::Error> = names
            .iter()
            .map(|n| Ok((n.clone(), r.get(n.as_str())?)))
            .collect();
        Ok((r.get("id")?, row?))
    });
    rows.unwrap().map(Result::unwrap).collect()
}

/// The row of a seed made at `now`: every column.
fn seed_row(seed: &KeyParts, seq: i64, now: i64) -> BTreeMap<String, Value> {
    let text = |s: &str| Value::Text(s.to_owned());
    let columns = [
        ("seq", Value::Integer(seq)),
        ("id", text(&seed.id.to_string())),
        ("environment_id", text(&env(1).id().to_string())),
        ("environment_token", text(&env(1).token().to_string())),
        ("scopes", text("read,write,admin")),
        ("digest", Value::Blob(seed.digest.to_stored().to_vec())),
        ("origin", text("seed")),
        ("created_by", Value::Null),
        ("label", Value::Null),
        ("expires_at", Value::Null),
        ("expiry_recorded", Value::Integer(0)),
        ("revoked_at", Value::Null),
        ("created_at", Value::Integer(now)),
    ];
    columns.map(|(name, value)| (name.to_owned(), value)).into()
}

/// Every unrevoked seed's ID.
fn current_seeds(cp: &ControlPlane) -> Vec<KeyId> {
    let rows = cp.list_keys(&env(1), None, 2_000).unwrap();
    rows.into_iter()
        .filter(|r| r.origin == KeyOrigin::Seed && r.record.revoked_at.is_none())
        .map(|r| r.record.id)
        .collect()
}

#[test]
fn a_seeded_bootstrap_holds_the_seed_as_its_one_admin_key() {
    let dir = tempfile::tempdir().unwrap();
    let cp = ControlPlane::open(&path(dir.path())).unwrap();
    let a = seed(110, 1);
    let b = Bootstrap {
        key: a.clone(),
        by_seed: true,
        ..bootstrap(env(1), 0).0
    };
    cp.ensure_bootstrap(&b, &Fixed(T)).unwrap();
    let rows = cp.list_keys(&env(1), None, 10).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        (
            rows[0].record.id,
            rows[0].origin,
            rows[0].created_by.clone()
        ),
        (a.id, KeyOrigin::Seed, None)
    );
    let every = BTreeSet::from([Scope::Read, Scope::Write, Scope::Admin]);
    assert_eq!(rows[0].record.scopes, every);
    assert!(rows[0].record.digest.ct_eq(&a.digest));
    assert_eq!(
        cp.apply_seed(&env(1), &a, &Fixed(T + 1)).unwrap(),
        SeedState::Current
    );
}

/// A new seed revokes the current one and becomes current, holding all three scopes and no
/// expiry; other admin keys, and seeds revoked before, keep what they were. A seed applied
/// again, current or revoked, changes nothing (D33). At every step one seed is unrevoked.
#[test]
fn a_new_seed_rotates_the_current_one_and_an_old_one_stays_revoked() {
    let dir = tempfile::tempdir().unwrap();
    let cp = ControlPlane::open(&path(dir.path())).unwrap();
    let (b, minted) = bootstrap(env(1), 0);
    cp.ensure_bootstrap(&b, &Fixed(T)).unwrap();
    let made = new_key(111, &[Scope::Admin], None, None);
    let made_id = made.id;
    cp.insert_key(&env(1), made, &Emitter::from(minted.id), &Fixed(T), CAP)
        .unwrap();
    let (a, b) = (seed(112, 1), seed(113, 1));
    let apply = |s: &KeyParts, now: i64| cp.apply_seed(&env(1), s, &Fixed(now)).unwrap();
    let mut expected = whole_rows(dir.path());
    // The rows are `expected` with `new` added, a seed made at `now`, and `revoked` revoked then.
    let mut rotated = |new: &KeyParts, revoked: Option<&KeyParts>, now: i64, case: &str| {
        if let Some(old) = revoked {
            let old = expected.get_mut(&old.id.to_string()).unwrap();
            old.insert("revoked_at".into(), Value::Integer(now));
        }
        let seq = i64::try_from(expected.len()).unwrap() + 1;
        expected.insert(new.id.to_string(), seed_row(new, seq, now));
        assert_eq!(whole_rows(dir.path()), expected, "{case}");
        assert_eq!(current_seeds(&cp), [new.id], "{case}");
    };

    assert_eq!(apply(&a, T + 1), SeedState::Current);
    rotated(&a, None, T + 1, "the first seed");
    let before = whole_rows(dir.path());
    let calls = ironflowx_authz::ct_eq_calls();
    assert_eq!(apply(&a, T + 2), SeedState::Current);
    assert_eq!(
        ironflowx_authz::ct_eq_calls() - calls,
        1,
        "compared (TEN-4)"
    );
    assert_eq!(whole_rows(dir.path()), before, "the current seed again");

    assert_eq!(apply(&b, T + 3), SeedState::Current);
    rotated(&b, Some(&a), T + 3, "a new seed");
    let before = whole_rows(dir.path());
    let calls = ironflowx_authz::ct_eq_calls();
    assert_eq!(apply(&a, T + 4), SeedState::Revoked);
    assert_eq!(
        ironflowx_authz::ct_eq_calls() - calls,
        1,
        "compared (TEN-4)"
    );
    assert_eq!(whole_rows(dir.path()), before, "a revoked seed");

    // With no current seed left, a new one revokes nothing, and `a` keeps its first time.
    cp.revoke_key(&env(1), b.id, &Emitter::from(made_id), &Fixed(T + 5))
        .unwrap();
    let before = whole_rows(dir.path());
    let c = seed(114, 1);
    assert_eq!(apply(&c, T + 6), SeedState::Current);
    let mut rows = whole_rows(dir.path());
    rows.remove(&c.id.to_string()).expect("the new seed");
    assert_eq!(rows, before, "after the last seed was revoked");
    assert_eq!(current_seeds(&cp), [c.id]);
}

/// A seed whose key ID is a key already, with another digest or another origin, or of another
/// incarnation, is a conflict: no key changes.
#[test]
fn a_seed_whose_key_id_is_another_key_is_a_conflict() {
    let dir = tempfile::tempdir().unwrap();
    let cp = ControlPlane::open(&path(dir.path())).unwrap();
    let (b, minted) = bootstrap(env(1), 0);
    cp.ensure_bootstrap(&b, &Fixed(T)).unwrap();
    let made = new_key(115, &[Scope::Admin], None, None);
    let made_parts = KeyParts {
        id: made.id,
        digest: made.digest.clone(),
    };
    cp.insert_key(&env(1), made, &Emitter::from(minted.id), &Fixed(T), CAP)
        .unwrap();
    let (a, old) = (seed(116, 1), seed(117, 1));
    for s in [&old, &a] {
        cp.apply_seed(&env(1), s, &Fixed(T)).unwrap();
    }
    // `compared`: how many digests are compared, each through `ct_eq` (TEN-4).
    let conflicts = |s: &KeyParts, compared: usize, case: &str| {
        let before = whole_rows(dir.path());
        let calls = ironflowx_authz::ct_eq_calls();
        let got = cp.apply_seed(&env(1), s, &Fixed(T + 1));
        assert!(matches!(got, Err(SeedError::Conflict)), "{case}: {got:?}");
        assert_eq!(whole_rows(dir.path()), before, "{case}");
        assert_eq!(ironflowx_authz::ct_eq_calls() - calls, compared, "{case}");
        assert_eq!(current_seeds(&cp), [a.id], "{case}");
        let calls = ironflowx_authz::ct_eq_calls();
        let got = ControlPlane::check_seed(&path(dir.path()), &env(1), s);
        assert!(matches!(got, Err(SeedError::Conflict)), "{case}: {got:?}");
        assert_eq!(ironflowx_authz::ct_eq_calls() - calls, compared, "{case}");
        assert_eq!(whole_rows(dir.path()), before, "{case}");
    };
    conflicts(&seed(116, 2), 1, "the current seed's ID, another secret");
    conflicts(&seed(117, 2), 1, "a revoked seed's ID, another secret");
    conflicts(&made_parts, 1, "a created key's own text");
    conflicts(&seed(115, 9), 1, "a created key's ID, another secret");
    conflicts(&minted, 1, "the minted key's own text");
    // A seed that would apply is no conflict, and checking it applies nothing.
    let before = whole_rows(dir.path());
    for fine in [&a, &old, &seed(118, 1)] {
        ControlPlane::check_seed(&path(dir.path()), &env(1), fine).expect("no conflict");
    }
    assert_eq!(whole_rows(dir.path()), before);

    let db = rusqlite::Connection::open(path(dir.path())).unwrap();
    let set = |origin: &str, token: &str| {
        db.execute(
            "UPDATE keys SET origin = ?1, environment_token = ?2 WHERE id = ?3",
            [origin, token, &made_parts.id.to_string()],
        )
        .unwrap();
    };
    set("recovered", &env(1).token().to_string());
    conflicts(&made_parts, 1, "a recovered key's own text");
    set("seed", &format!("{:032x}", 8));
    conflicts(&made_parts, 0, "a seed of another incarnation");
}

/// A failed change leaves the previous seed current: the revoke and the insert are one
/// transaction (TEN-11).
#[test]
fn a_seed_change_that_fails_keeps_the_previous_seed() {
    let dir = tempfile::tempdir().unwrap();
    let cp = ControlPlane::open(&path(dir.path())).unwrap();
    cp.ensure_bootstrap(&bootstrap(env(1), 0).0, &Fixed(T))
        .unwrap();
    let (a, b) = (seed(118, 1), seed(119, 1));
    cp.apply_seed(&env(1), &a, &Fixed(T)).unwrap();
    let before = whole_rows(dir.path());
    let db = rusqlite::Connection::open(path(dir.path())).unwrap();
    db.execute_batch(
        "CREATE TRIGGER fail BEFORE INSERT ON keys BEGIN SELECT RAISE(ABORT, 'injected'); END;",
    )
    .unwrap();
    let failed = cp.apply_seed(&env(1), &b, &Fixed(T + 1));
    assert!(matches!(failed, Err(SeedError::Store(_))), "{failed:?}");
    assert_eq!(whole_rows(dir.path()), before);
    db.execute_batch("DROP TRIGGER fail").unwrap();
    cp.apply_seed(&env(1), &b, &Fixed(T + 2)).unwrap();
    assert_eq!(current_seeds(&cp), [b.id]);
}

/// Seeds hold no slot: they apply at a full cap, and free none (TEN-15).
#[test]
fn a_seed_holds_no_slot() {
    let dir = tempfile::tempdir().unwrap();
    let cp = ControlPlane::open(&path(dir.path())).unwrap();
    let (b, admin) = bootstrap(env(1), 0);
    cp.ensure_bootstrap(&b, &Fixed(T)).unwrap();
    let create = |n: u8| {
        let new = new_key(n, &[Scope::Read], None, None);
        cp.insert_key(&env(1), new, &Emitter::from(admin.id), &Fixed(T), 2)
    };
    cp.apply_seed(&env(1), &seed(120, 1), &Fixed(T)).unwrap();
    create(121).expect("the first slot");
    create(122).expect("the cap itself");
    cp.apply_seed(&env(1), &seed(123, 1), &Fixed(T)).unwrap();
    let full = create(124);
    assert!(matches!(full, Err(KeyChangeError::Full)), "{full:?}");
}

/// A rotation revokes the current seed of its own environment and incarnation only.
#[test]
fn a_rotation_leaves_the_seeds_of_other_environments_and_incarnations() {
    let dir = tempfile::tempdir().unwrap();
    let cp = ControlPlane::open(&path(dir.path())).unwrap();
    cp.ensure_bootstrap(&bootstrap(env(1), 0).0, &Fixed(T))
        .unwrap();
    let db = rusqlite::Connection::open(path(dir.path())).unwrap();
    // The planted rows name environments this file does not hold.
    db.pragma_update(None, "foreign_keys", false).unwrap();
    let other_token: EnvRef = format!("{}.{:032x}", env(1).id(), 8).parse().unwrap();
    for (n, elsewhere) in [(130, env(2)), (131, other_token)] {
        let moved = seed(n, 1);
        cp.apply_seed(&env(1), &moved, &Fixed(T)).unwrap();
        db.execute(
            "UPDATE keys SET environment_id = ?1, environment_token = ?2 WHERE id = ?3",
            [
                elsewhere.id().to_string(),
                elsewhere.token().to_string(),
                moved.id.to_string(),
            ],
        )
        .unwrap();
        cp.apply_seed(&env(1), &seed(n + 10, 1), &Fixed(T)).unwrap();
        let kept = found(&cp, moved.id, &elsewhere).expect("the moved row");
        assert_eq!(kept.revoked_at, None, "{elsewhere}");
    }
}

/// A clock that says when it is read.
struct Told(Sender<()>);

impl Clock for Told {
    fn now(&self) -> Timestamp {
        let _ = self.0.send(());
        Timestamp::from_millis(T)
    }
}

/// How `change` ends on a bootstrapped control plane, marked or not, whose write lock another
/// connection holds until it comes to its own `BEGIN IMMEDIATE`, not having read the clock; it
/// reads it once the lock is free.
fn under_a_held_lock<R: Send>(
    marked: bool,
    change: impl FnOnce(&ControlPlane, Told) -> R + Send,
) -> R {
    writes::trace();
    let dir = tempfile::tempdir().unwrap();
    let cp = ControlPlane::open(&path(dir.path())).unwrap();
    cp.ensure_bootstrap(&bootstrap(env(1), 0).0, &Fixed(T))
        .unwrap();
    if marked {
        cp.mark_ledger_created(&env(1), &Fixed(T)).unwrap();
    }
    let db = rusqlite::Connection::open(path(dir.path())).unwrap();
    db.execute_batch("BEGIN IMMEDIATE").unwrap();
    let begun = || writes::begun(&path(dir.path()));
    let before = begun();
    let (tell, told) = channel();
    std::thread::scope(|s| {
        let changing = s.spawn(|| change(&cp, Told(tell)));
        let until = Instant::now() + Duration::from_secs(3);
        while begun() == before && Instant::now() < until {
            std::thread::yield_now();
        }
        let (came, early) = (begun() - before, told.try_recv());
        db.execute_batch("COMMIT").unwrap();
        assert_eq!(came, 1, "it did not come to its BEGIN IMMEDIATE");
        early.expect_err("the clock was read before the lock");
        let ended = changing.join().unwrap();
        told.recv().expect("read once the lock is free");
        ended
    })
}

/// The seed's transaction takes the write lock before it reads the clock (TEN-11): while another
/// connection holds the lock, the clock is not read.
#[test]
fn a_seed_is_applied_at_a_now_read_after_the_write_lock() {
    let applied = under_a_held_lock(false, |cp, told| {
        cp.apply_seed(&env(1), &seed(150, 1), &told)
    });
    assert_eq!(applied.unwrap(), SeedState::Current);
}

/// An environment, or an incarnation, the file does not hold takes no seed: it would be a second
/// unrevoked one, which no key of the engine's could be.
#[test]
fn a_seed_for_an_environment_the_file_does_not_hold_is_not_applied() {
    let dir = tempfile::tempdir().unwrap();
    let cp = ControlPlane::open(&path(dir.path())).unwrap();
    cp.ensure_bootstrap(&bootstrap(env(1), 0).0, &Fixed(T))
        .unwrap();
    let a = seed(160, 1);
    cp.apply_seed(&env(1), &a, &Fixed(T)).unwrap();
    let before = whole_rows(dir.path());
    let other_token: EnvRef = format!("{}.{:032x}", env(1).id(), 8).parse().unwrap();
    for elsewhere in [other_token, env(2)] {
        for s in [&a, &seed(161, 1)] {
            let got = cp.apply_seed(&elsewhere, s, &Fixed(T + 1));
            assert!(matches!(got, Err(SeedError::Store(_))), "{got:?}");
            let got = ControlPlane::check_seed(&path(dir.path()), &elsewhere, s);
            assert!(matches!(got, Err(SeedError::Store(_))), "{got:?}");
            assert_eq!(whole_rows(dir.path()), before, "{elsewhere}");
        }
    }
}

/// Every row of every table but `meta`, as text.
fn everything(dir: &Path) -> Vec<String> {
    let db = rusqlite::Connection::open(path(dir)).unwrap();
    let mut out = Vec::new();
    for table in ["tenants", "projects", "environments", "keys"] {
        let mut stmt = db.prepare(&format!("SELECT * FROM {table}")).unwrap();
        let columns = stmt.column_count();
        let rows = stmt.query_map([], |r| {
            let row: Result<Vec<Value>, _> = (0..columns).map(|c| r.get(c)).collect();
            Ok(format!("{table} {:?}", row?))
        });
        out.extend(rows.unwrap().map(Result::unwrap));
    }
    out
}

/// When `env(1)` was marked, if it was.
fn marked_at(dir: &Path) -> Option<i64> {
    let db = rusqlite::Connection::open(path(dir)).unwrap();
    let sql = "SELECT ledger_created_at FROM environments";
    db.query_row(sql, [], |r| r.get(0)).unwrap()
}

/// `env(1)`'s ID with another incarnation token.
fn other_token() -> EnvRef {
    format!("{}.{:032x}", env(1).id(), 8).parse().unwrap()
}

/// Undoing deletes all the bootstrap made, so the file bootstraps again; not once it is marked,
/// and nothing for an environment or an incarnation the file does not hold (D41).
#[test]
fn an_undo_deletes_an_unmarked_bootstrap_and_nothing_else() {
    let dir = tempfile::tempdir().unwrap();
    let cp = ControlPlane::open(&path(dir.path())).unwrap();
    cp.undo_bootstrap(&env(1)).expect("nothing to undo");
    cp.ensure_bootstrap(&bootstrap(env(1), 0).0, &Fixed(T))
        .unwrap();
    let before = everything(dir.path());
    assert_eq!(before.len(), 4);
    for elsewhere in [env(2), other_token()] {
        cp.undo_bootstrap(&elsewhere).expect("nothing to undo");
        assert_eq!(everything(dir.path()), before, "{elsewhere}");
    }
    cp.undo_bootstrap(&env(1)).unwrap();
    assert_eq!(everything(dir.path()), [""; 0]);
    let state = ControlPlane::bootstrap_state(&path(dir.path())).unwrap();
    assert_eq!(state, BootstrapState::Unbootstrapped);

    cp.ensure_bootstrap(&bootstrap(env(1), 1).0, &Fixed(T))
        .expect("bootstraps again");
    cp.mark_ledger_created(&env(1), &Fixed(T)).unwrap();
    let before = everything(dir.path());
    cp.undo_bootstrap(&env(1)).expect_err("marked");
    assert_eq!(everything(dir.path()), before);
}

/// The undo is one transaction: failing at any of its deletes, it deletes nothing (TEN-11).
#[test]
fn an_undo_that_fails_deletes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let cp = ControlPlane::open(&path(dir.path())).unwrap();
    cp.ensure_bootstrap(&bootstrap(env(1), 0).0, &Fixed(T))
        .unwrap();
    let before = everything(dir.path());
    let db = rusqlite::Connection::open(path(dir.path())).unwrap();
    for table in ["keys", "environments", "projects", "tenants"] {
        db.execute_batch(&format!(
            "CREATE TRIGGER fail BEFORE DELETE ON {table} \
             BEGIN SELECT RAISE(ABORT, 'injected: {table}'); END;"
        ))
        .unwrap();
        // The trigger's own error: a missing delete would fail at a later foreign key instead.
        match cp.undo_bootstrap(&env(1)).expect_err(table) {
            StoreError::Failed(why) => assert_eq!(why, format!("injected: {table}")),
            err => panic!("{table}: {err:?}"),
        }
        assert_eq!(everything(dir.path()), before, "{table}");
        db.execute_batch("DROP TRIGGER fail").unwrap();
    }
}

/// Finishing marks the bootstrap and, with a minted key, revokes it and stores its replacement,
/// if there is one; a seeded bootstrap keeps its seed and stores no replacement (D41).
#[test]
fn finishing_revokes_a_minted_key_and_replaces_it_but_keeps_a_seed() {
    let replacement = seed(170, 1);
    for (by_seed, replace) in [(false, true), (false, false), (true, true), (true, false)] {
        let dir = tempfile::tempdir().unwrap();
        let cp = ControlPlane::open(&path(dir.path())).unwrap();
        let (b, first) = bootstrap(env(1), 0);
        let b = Bootstrap { by_seed, ..b };
        cp.ensure_bootstrap(&b, &Fixed(T)).unwrap();
        let mut expected = whole_rows(dir.path());
        let given = replace.then_some(&replacement);
        let lost = cp.finish_bootstrap(&env(1), given, &Fixed(T + 1)).unwrap();
        assert_eq!(lost, !by_seed);
        if lost {
            let old = expected.get_mut(&first.id.to_string()).unwrap();
            old.insert("revoked_at".into(), Value::Integer(T + 1));
        }
        if lost && replace {
            let mut row = seed_row(&replacement, 2, T + 1);
            row.insert("origin".into(), Value::Text("minted".into()));
            expected.insert(replacement.id.to_string(), row);
        }
        assert_eq!(whole_rows(dir.path()), expected, "{by_seed} {replace}");
        assert_eq!(marked_at(dir.path()), Some(T + 1));
    }
}

/// Marking, or finishing, happens once, and only for the file's own environment: a finished
/// bootstrap's minted key is in use, and finishing again would revoke it.
#[test]
fn a_bootstrap_is_marked_once() {
    let dir = tempfile::tempdir().unwrap();
    let cp = ControlPlane::open(&path(dir.path())).unwrap();
    cp.ensure_bootstrap(&bootstrap(env(1), 0).0, &Fixed(T))
        .unwrap();
    let refused = |env: &EnvRef, case: &str| {
        let before = everything(dir.path());
        let replacement = seed(171, 1);
        cp.mark_ledger_created(env, &Fixed(T + 2)).expect_err(case);
        cp.finish_bootstrap(env, Some(&replacement), &Fixed(T + 2))
            .expect_err(case);
        assert_eq!(everything(dir.path()), before, "{case}");
    };
    refused(&env(2), "another environment");
    refused(&other_token(), "another incarnation");
    cp.mark_ledger_created(&env(1), &Fixed(T + 1)).unwrap();
    assert_eq!(marked_at(dir.path()), Some(T + 1));
    refused(&env(1), "marked");
}

/// Finishing is one transaction: if the replacement cannot be stored, or the mark cannot, nothing
/// is revoked or marked (TEN-11).
#[test]
fn a_finish_that_fails_changes_nothing() {
    for point in ["INSERT ON keys", "UPDATE ON environments", "UPDATE ON keys"] {
        let dir = tempfile::tempdir().unwrap();
        let cp = ControlPlane::open(&path(dir.path())).unwrap();
        cp.ensure_bootstrap(&bootstrap(env(1), 0).0, &Fixed(T))
            .unwrap();
        let before = everything(dir.path());
        let db = rusqlite::Connection::open(path(dir.path())).unwrap();
        db.execute_batch(&format!(
            "CREATE TRIGGER fail BEFORE {point} BEGIN SELECT RAISE(ABORT, 'injected'); END;"
        ))
        .unwrap();
        let replacement = seed(172, 1);
        cp.finish_bootstrap(&env(1), Some(&replacement), &Fixed(T + 1))
            .expect_err(point);
        assert_eq!(everything(dir.path()), before, "{point}");
    }
}

/// A finish revokes the minted key only: a key some other way of making one stored stays.
#[test]
fn finishing_revokes_no_key_of_another_origin_or_environment() {
    let dir = tempfile::tempdir().unwrap();
    let cp = ControlPlane::open(&path(dir.path())).unwrap();
    let (b, minted) = bootstrap(env(1), 0);
    cp.ensure_bootstrap(&b, &Fixed(T)).unwrap();
    let db = rusqlite::Connection::open(path(dir.path())).unwrap();
    db.pragma_update(None, "foreign_keys", false).unwrap();
    for (n, origin, at) in [
        (173, "seed", env(1)),
        (174, "created", env(1)),
        (175, "recovered", env(1)),
        (176, "minted", env(2)),
        (177, "minted", other_token()),
    ] {
        let new = new_key(n, &[Scope::Admin], None, None);
        let id = new.id.to_string();
        cp.insert_key(&env(1), new, &Emitter::from(minted.id), &Fixed(T), CAP)
            .unwrap();
        db.execute(
            "UPDATE keys SET origin = ?1, environment_id = ?2, environment_token = ?3 \
             WHERE id = ?4",
            [origin, &at.id().to_string(), &at.token().to_string(), &id],
        )
        .unwrap();
    }
    assert!(cp.finish_bootstrap(&env(1), None, &Fixed(T + 1)).unwrap());
    let revoked: Vec<_> = whole_rows(dir.path())
        .into_iter()
        .filter(|(_, row)| row["revoked_at"] != Value::Null)
        .map(|(id, _)| id)
        .collect();
    assert_eq!(revoked, [minted.id.to_string()]);
}

/// Marking and finishing take the write lock before they read the clock (TEN-11).
#[test]
fn a_mark_is_made_at_a_now_read_after_the_write_lock() {
    let marked = under_a_held_lock(false, |cp, told| cp.mark_ledger_created(&env(1), &told));
    marked.unwrap();
    let lost = under_a_held_lock(false, |cp, told| cp.finish_bootstrap(&env(1), None, &told));
    assert!(lost.unwrap());
}

/// A recovered key is an admin key of the file's environment, of that origin and label, every
/// column compared, and no other row changes (D34).
#[test]
fn a_recovered_key_is_a_labelled_admin_key_and_changes_no_other_row() {
    let dir = tempfile::tempdir().unwrap();
    let cp = ControlPlane::open(&path(dir.path())).unwrap();
    let (b, admin) = bootstrap(env(1), 0);
    cp.ensure_bootstrap(&b, &Fixed(T)).unwrap();
    cp.mark_ledger_created(&env(1), &Fixed(T)).unwrap();
    // A key of every origin, the second recovery finding a recovered one.
    cp.apply_seed(&env(1), &seed(178, 1), &Fixed(T)).unwrap();
    let new = new_key(179, &[Scope::Read], None, None);
    cp.insert_key(&env(1), new, &Emitter::from(admin.id), &Fixed(T), CAP)
        .unwrap();
    for (seq, n) in [(4, 180), (5, 181)] {
        let mut expected = whole_rows(dir.path());
        let key = seed(n, 1);
        cp.insert_recovered(&key, &Fixed(T + seq)).unwrap();
        let mut row = seed_row(&key, seq, T + seq);
        for column in ["origin", "label"] {
            row.insert(column.into(), Value::Text("recovered".into()));
        }
        expected.insert(key.id.to_string(), row);
        assert_eq!(whole_rows(dir.path()), expected);
    }
}

/// Until the bootstrap is marked, whether never made, unmarked or undone, nothing is recovered,
/// so an undo never meets a recovered key (TEN-14).
#[test]
fn nothing_is_recovered_until_the_bootstrap_is_marked() {
    let dir = tempfile::tempdir().unwrap();
    let cp = ControlPlane::open(&path(dir.path())).unwrap();
    let key = seed(182, 1);
    let refused = |case: &str| {
        let before = everything(dir.path());
        let got = cp.insert_recovered(&key, &Fixed(T));
        assert!(matches!(got, Err(RecoverError::NotReady)), "{case}");
        assert_eq!(everything(dir.path()), before, "{case}");
    };
    refused("never bootstrapped");
    cp.ensure_bootstrap(&bootstrap(env(1), 0).0, &Fixed(T))
        .unwrap();
    refused("unmarked");
    cp.undo_bootstrap(&env(1)).unwrap();
    refused("undone");
    cp.ensure_bootstrap(&bootstrap(env(1), 0).0, &Fixed(T))
        .unwrap();
    cp.mark_ledger_created(&env(1), &Fixed(T)).unwrap();
    cp.insert_recovered(&key, &Fixed(T)).expect("marked");
}

fn recovery_failed(got: Result<(), RecoverError>, why: &str) {
    let failed = matches!(&got, Err(RecoverError::Store(StoreError::Failed(w))) if w == why);
    assert!(failed, "{why}: {got:?}");
}

/// A recovery that cannot read whether the bootstrap is marked, or come to, fails as that, not as
/// not ready, and stores nothing.
#[test]
fn a_recovery_that_cannot_decide_is_a_store_error() {
    let dir = tempfile::tempdir().unwrap();
    let cp = ControlPlane::open(&path(dir.path())).unwrap();
    cp.ensure_bootstrap(&bootstrap(env(1), 0).0, &Fixed(T))
        .unwrap();
    cp.mark_ledger_created(&env(1), &Fixed(T)).unwrap();
    let before = whole_rows(dir.path());
    let db = rusqlite::Connection::open(path(dir.path())).unwrap();
    let key = seed(182, 2);
    let recover = || cp.insert_recovered(&key, &Fixed(T));

    let token = |t: &str| {
        let sql = "UPDATE environments SET token = ?1";
        assert_eq!(db.execute(sql, [t]).unwrap(), 1);
    };
    token("not a token");
    recovery_failed(recover(), "bad environment reference");
    token(&env(1).token().to_string());

    db.execute_batch("BEGIN IMMEDIATE").unwrap();
    recovery_failed(recover(), "database is locked");
    db.execute_batch("COMMIT").unwrap();

    struct Panics;
    impl Clock for Panics {
        fn now(&self) -> Timestamp {
            panic!("injected");
        }
    }
    std::thread::scope(|s| {
        let panicked = s.spawn(|| cp.insert_recovered(&key, &Panics)).join();
        panicked.expect_err("it holds the connection as it panics");
    });
    recovery_failed(recover(), "poisoned");
    assert_eq!(whole_rows(dir.path()), before);
}

/// A recovery that fails leaves no row: at its insert, or at its commit, the row inserted by then
/// (TEN-11). Failed, the same key is recovered by a later call.
#[test]
fn a_recovery_that_fails_leaves_no_row() {
    // The second one's row breaks a constraint that is checked only at the commit.
    let insert = "CREATE TRIGGER fail BEFORE INSERT ON keys \
                  BEGIN SELECT RAISE(ABORT, 'injected'); END;";
    let commit = "CREATE TABLE late (t TEXT REFERENCES tenants(id) DEFERRABLE INITIALLY DEFERRED);
                  CREATE TRIGGER fail AFTER INSERT ON keys BEGIN \
                  INSERT INTO late VALUES ('no such tenant'); END;";
    for (point, sql, why) in [
        ("insert", insert, "injected"),
        ("commit", commit, "FOREIGN KEY constraint failed"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let cp = ControlPlane::open(&path(dir.path())).unwrap();
        cp.ensure_bootstrap(&bootstrap(env(1), 0).0, &Fixed(T))
            .unwrap();
        cp.mark_ledger_created(&env(1), &Fixed(T)).unwrap();
        let before = everything(dir.path());
        let db = rusqlite::Connection::open(path(dir.path())).unwrap();
        db.execute_batch(sql).unwrap();
        let key = seed(183, 1);
        recovery_failed(cp.insert_recovered(&key, &Fixed(T)), why);
        assert_eq!(everything(dir.path()), before, "{point}");
        db.execute_batch("DROP TRIGGER fail").unwrap();
        cp.insert_recovered(&key, &Fixed(T)).expect(point);
    }
}

/// A recovery takes the write lock before it reads the clock (TEN-11).
#[test]
fn a_key_is_recovered_at_a_now_read_after_the_write_lock() {
    let recovered = under_a_held_lock(true, |cp, told| cp.insert_recovered(&seed(184, 1), &told));
    recovered.unwrap();
}

/// A key is created, and one revoked, at a now read after the write lock, so a caller that expires
/// while it waits for the lock is refused (D43); a bootstrap reads its own so too (TEN-11).
#[test]
fn a_key_is_created_and_revoked_at_a_now_read_after_the_write_lock() {
    let admin = bootstrap(env(1), 0).1.id;
    let created = under_a_held_lock(true, |cp, told| {
        let new = new_key(196, &[Scope::Read], None, None);
        cp.insert_key(&env(1), new, &Emitter::from(admin), &told, CAP)
            .map(drop)
    });
    created.unwrap();
    let revoked = under_a_held_lock(true, |cp, told| {
        cp.revoke_key(&env(1), admin, &Emitter::from(admin), &told)
    });
    revoked.unwrap();
    // Refused, the file being bootstrapped, but not before it read the clock.
    let again = under_a_held_lock(false, |cp, told| {
        cp.ensure_bootstrap(&bootstrap(env(2), 1).0, &told)
    });
    let refused = matches!(&again, Err(StoreError::Failed(why)) if why == "already bootstrapped");
    assert!(refused, "{again:?}");
}

/// Recovered keys hold no slot: at a full cap one is recovered and a seed then applies, and
/// neither takes nor frees a slot (TEN-15).
#[test]
fn a_recovered_key_holds_no_slot() {
    let dir = tempfile::tempdir().unwrap();
    let cp = ControlPlane::open(&path(dir.path())).unwrap();
    let (b, admin) = bootstrap(env(1), 0);
    cp.ensure_bootstrap(&b, &Fixed(T)).unwrap();
    cp.mark_ledger_created(&env(1), &Fixed(T)).unwrap();
    let create = |n: u8| {
        let new = new_key(n, &[Scope::Read], None, None);
        cp.insert_key(&env(1), new, &Emitter::from(admin.id), &Fixed(T), 2)
    };
    cp.insert_recovered(&seed(190, 1), &Fixed(T)).unwrap();
    create(191).expect("the first slot");
    create(192).expect("the cap itself");
    cp.insert_recovered(&seed(193, 1), &Fixed(T)).unwrap();
    let applied = cp.apply_seed(&env(1), &seed(194, 1), &Fixed(T)).unwrap();
    assert_eq!(applied, SeedState::Current);
    let full = create(195);
    assert!(matches!(full, Err(KeyChangeError::Full)), "{full:?}");
}

/// A file in another format is refused as that; one that is not a control plane is refused and
/// left as it is, not given a schema; one with nothing in it is not bootstrapped yet.
#[test]
fn a_file_is_opened_only_if_it_is_this_formats_control_plane_or_empty() {
    let dir = tempfile::tempdir().unwrap();
    let path = path(dir.path());
    let refused = |case: &str, format: bool| {
        let before = std::fs::read(&path).unwrap();
        let read = ControlPlane::bootstrap_state(&path).expect_err(case);
        let opened = ControlPlane::open(&path).err().expect(case);
        for err in [read, opened] {
            assert_eq!(matches!(err, StoreError::Format), format, "{case}: {err}");
        }
        assert!(std::fs::read(&path).unwrap() == before, "{case}");
    };
    for format in ["0", "2", "-1", "65537", "4294967297"] {
        drop(ControlPlane::open(&path).unwrap());
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute_batch(&format!("UPDATE meta SET format = {format}"))
            .unwrap();
        drop(db);
        refused(format, true);
        std::fs::remove_file(&path).unwrap();
    }
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch("CREATE TABLE notes (text TEXT)").unwrap();
    drop(db);
    refused("another program's file", false);
    std::fs::write(&path, "not a database at all").unwrap();
    refused("not a database", false);

    std::fs::write(&path, "").unwrap();
    assert_eq!(
        ControlPlane::bootstrap_state(&path).unwrap(),
        BootstrapState::Unbootstrapped
    );
    ControlPlane::open(&path).expect("an empty file takes the schema");
}

/// Format 1 is this schema (D47). To change it is to change `FORMAT` too, so that a file made
/// before is refused, not misread.
#[test]
fn format_1_is_this_schema() {
    let dir = tempfile::tempdir().unwrap();
    drop(ControlPlane::open(&path(dir.path())).unwrap());
    let db = rusqlite::Connection::open(path(dir.path())).unwrap();
    let mut stmt = db
        .prepare("SELECT sql FROM sqlite_master WHERE sql IS NOT NULL ORDER BY rowid")
        .unwrap();
    let sql = stmt.query_map([], |r| r.get::<_, String>(0)).unwrap();
    let one_line = |s: String| s.split_whitespace().collect::<Vec<_>>().join(" ");
    let schema: Vec<_> = sql.map(|s| one_line(s.unwrap())).collect();
    assert_eq!(
        schema,
        [
            "CREATE TABLE meta (format INTEGER NOT NULL)",
            "CREATE TABLE tenants (id TEXT PRIMARY KEY, name TEXT NOT NULL, \
             created_at INTEGER NOT NULL)",
            "CREATE TABLE projects ( id TEXT PRIMARY KEY, \
             tenant_id TEXT NOT NULL REFERENCES tenants(id), name TEXT NOT NULL, \
             created_at INTEGER NOT NULL )",
            "CREATE TABLE environments ( id TEXT PRIMARY KEY, \
             project_id TEXT NOT NULL REFERENCES projects(id), name TEXT NOT NULL, \
             token TEXT NOT NULL, created_at INTEGER NOT NULL, ledger_created_at INTEGER )",
            "CREATE TABLE keys ( seq INTEGER PRIMARY KEY, id TEXT NOT NULL UNIQUE, \
             environment_id TEXT NOT NULL REFERENCES environments(id), \
             environment_token TEXT NOT NULL, scopes TEXT NOT NULL, digest BLOB NOT NULL, \
             origin TEXT NOT NULL, created_by TEXT, label TEXT, expires_at INTEGER, \
             expiry_recorded INTEGER NOT NULL DEFAULT 0, revoked_at INTEGER, \
             created_at INTEGER NOT NULL )",
        ]
    );
    let format: i64 = db
        .query_row("SELECT format FROM meta", [], |r| r.get(0))
        .unwrap();
    assert_eq!(format, 1);
}

/// Reading the state leaves no file behind (the directory may be one the engine refuses).
#[test]
fn reading_the_state_leaves_no_file_behind() {
    let dir = tempfile::tempdir().unwrap();
    let cp = ControlPlane::open(&path(dir.path())).unwrap();
    cp.ensure_bootstrap(&bootstrap(env(1), 0).0, &Fixed(T))
        .unwrap();
    drop(cp);
    let before = std::fs::read(path(dir.path())).unwrap();
    ControlPlane::bootstrap_state(&path(dir.path())).unwrap();
    let names: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names, ["control.db"]);
    assert!(std::fs::read(path(dir.path())).unwrap() == before);
}
