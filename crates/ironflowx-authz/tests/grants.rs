//! What a verified key may do: its expiry, its scopes (D27, D31, D37) and what it may create
//! (D28, TEN-5), through the crate's public functions.

use std::collections::BTreeSet;

use ironflowx_authz::{Grant, KeyRecord, admit, may_create, mint, parse, recheck, verify};
use ironflowx_core::{EnvRef, KeyId, Scope, Timestamp};

const T: i64 = 1_800_000_000_000;
const HOUR: i64 = 3_600_000;

fn id() -> KeyId {
    "019a9f3e-7c2b-7d41-8a3e-5f6b7c8d9e0f".parse().unwrap()
}

fn env() -> EnvRef {
    "019a9f3e-7c2b-7d41-8a3e-000000000001.00112233445566778899aabbccddeeff"
        .parse()
        .unwrap()
}

const fn at(ms: i64) -> Timestamp {
    Timestamp::from_millis(ms)
}

fn record(scopes: &BTreeSet<Scope>, expires_at: Option<Timestamp>) -> KeyRecord {
    KeyRecord {
        id: id(),
        env: env(),
        scopes: scopes.clone(),
        digest: mint(id(), [7; 32]).1,
        expires_at,
        revoked_at: None,
    }
}

/// A grant for a key with `scopes` and `expires_at`, verified at `T`.
fn grant(scopes: &BTreeSet<Scope>, expires_at: Option<Timestamp>) -> Grant {
    let parts = parse(&mint(id(), [7; 32]).0).unwrap();
    verify(&parts, Some(&record(scopes, expires_at)), &env(), at(T)).unwrap()
}

/// Every set of scopes, the empty one first.
fn every_scope_set() -> Vec<BTreeSet<Scope>> {
    let all = [Scope::Read, Scope::Write, Scope::Admin];
    (0..8_u8)
        .map(|bits| {
            (0..3)
                .filter(|i| bits & (1 << i) != 0)
                .map(|i| all[i])
                .collect()
        })
        .collect()
}

#[test]
fn a_key_works_until_its_expiry_and_not_from_it() {
    let parts = parse(&mint(id(), [7; 32]).0).unwrap();
    let rec = record(&BTreeSet::from([Scope::Read]), Some(at(T)));
    let granted = verify(&parts, Some(&rec), &env(), at(T - 1)).unwrap();
    assert_eq!(granted.expires_at(), Some(at(T)));
    assert!(verify(&parts, Some(&rec), &env(), at(T)).is_err());
    assert!(verify(&parts, Some(&rec), &env(), at(T + 1)).is_err());
    // The same rule when a key change re-reads its creator's record.
    assert!(recheck(&rec, &env(), at(T - 1)).is_ok());
    assert!(recheck(&rec, &env(), at(T)).is_err());
    let other: EnvRef = "019a9f3e-7c2b-7d41-8a3e-000000000002.00112233445566778899aabbccddeeff"
        .parse()
        .unwrap();
    assert!(recheck(&rec, &other, at(T - 1)).is_err());
}

/// A revoked key is refused whatever its expiry, by verify and by recheck (TEN-2).
#[test]
fn a_revoked_key_is_refused() {
    let parts = parse(&mint(id(), [7; 32]).0).unwrap();
    for expires_at in [None, Some(at(T + HOUR))] {
        let rec = KeyRecord {
            revoked_at: Some(at(T - 1)),
            ..record(&BTreeSet::from([Scope::Admin]), expires_at)
        };
        assert!(
            verify(&parts, Some(&rec), &env(), at(T)).is_err(),
            "{expires_at:?}"
        );
        assert!(recheck(&rec, &env(), at(T)).is_err(), "{expires_at:?}");
    }
}

#[test]
fn a_grant_admits_only_the_scopes_it_holds() {
    for held in every_scope_set().into_iter().skip(1) {
        let g = grant(&held, None);
        assert_eq!(admit(&g, None), Ok(()), "any valid key: {held:?}");
        for scope in [Scope::Read, Scope::Write, Scope::Admin] {
            let admitted = admit(&g, Some(scope));
            if held.contains(&scope) {
                assert_eq!(admitted, Ok(()), "{held:?} {scope}");
            } else {
                let missing = admitted.expect_err("refused");
                assert_eq!(missing.scope(), scope, "{held:?}");
            }
        }
    }
}

/// Over every creator and every request: a created key never exceeds its creator in scope or
/// lifetime, and a request beyond the creator, or otherwise bad, is refused (TEN-5).
#[test]
fn no_created_key_exceeds_its_creator() {
    let creator_expiries = [None, Some(at(T + HOUR)), Some(at(T + 2 * HOUR))];
    let long = "x".repeat(128);
    let too_long = "x".repeat(129);
    // A two-byte character across the limit: bytes count, not characters.
    let wide_fits = format!("{}é", "x".repeat(126));
    let wide_over = format!("{}é", "x".repeat(127));
    let labels: [(Option<&str>, bool); 8] = [
        (None, true),
        (Some(""), true),
        (Some("agent-7"), true),
        (Some(&long), true),
        (Some(&too_long), false),
        (Some("a\nb"), false),
        (Some(&wide_fits), true),
        (Some(&wide_over), false),
    ];
    let mut checked = 0;
    for creator_scopes in every_scope_set().into_iter().skip(1) {
        for creator_expiry in creator_expiries {
            let creator = grant(&creator_scopes, creator_expiry);
            let mut expiries = vec![
                None,
                Some(at(T - 1)),
                Some(at(T)),
                Some(at(T + 1)),
                Some(at(T + HOUR)),
            ];
            if let Some(c) = creator_expiry {
                expiries.extend([Some(c), Some(at(c.as_millis() + 1))]);
            }
            for scopes in every_scope_set() {
                for &expires_at in &expiries {
                    for (label, label_ok) in labels {
                        let allowed = may_create(&creator, &scopes, expires_at, label, at(T));
                        let within_scope = !scopes.is_empty() && scopes.is_subset(&creator_scopes);
                        let within_lifetime = match (creator_expiry, expires_at) {
                            (None, _) => true,
                            (Some(_), None) => false,
                            (Some(c), Some(e)) => e <= c,
                        };
                        let after_now = expires_at.is_none_or(|e| e > at(T));
                        let expected = within_scope && within_lifetime && after_now && label_ok;
                        assert_eq!(
                            allowed.is_ok(),
                            expected,
                            "creator {creator_scopes:?} until {creator_expiry:?}; asked \
                             {scopes:?} until {expires_at:?}, label {label:?}: {allowed:?}"
                        );
                        checked += 1;
                    }
                }
            }
        }
    }
    // 7 creator scope sets × 19 creator and request expiry pairs × 8 requested sets × 8 labels.
    assert_eq!(checked, 7 * 19 * 8 * 8);
}
