//! What a verified key may do: its expiry, its scopes (D27, D31, D37) and what it may create
//! (D28, TEN-5), through the crate's public functions.

use std::collections::BTreeSet;

use ironflowx_authz::{Grant, KeyRecord, Refused, admit, may_create, mint, parse, recheck, verify};
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
        expiry_recorded: false,
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

/// Over every key and time: `newly_expired` says to record an expiry exactly when the digest
/// matches and the key, of this environment and neither revoked nor recorded before, has reached
/// its expiry; a recorded expiry refuses whatever the clock (TEN-13, D42).
#[test]
fn only_an_expiry_not_yet_recorded_is_newly_expired() {
    let right = parse(&mint(id(), [7; 32]).0).unwrap();
    let wrong = parse(&mint(id(), [8; 32]).0).unwrap();
    let other_env: EnvRef = "019a9f3e-7c2b-7d41-8a3e-000000000001.ffeeddccbbaa99887766554433221100"
        .parse()
        .unwrap();
    let verdict = |r: Result<Grant, Refused>| match r {
        Ok(_) => "granted",
        Err(r) if r.newly_expired() => "newly expired",
        Err(_) => "refused",
    };
    for now in [T - 1, T, T + 1] {
        for revoked in [false, true] {
            for recorded in [false, true] {
                for key_env in [env(), other_env] {
                    let rec = KeyRecord {
                        env: key_env,
                        expiry_recorded: recorded,
                        revoked_at: revoked.then_some(at(T - 5)),
                        ..record(&BTreeSet::from([Scope::Read]), Some(at(T)))
                    };
                    let case = format!("now {now}: {rec:?}");
                    let expected = match (revoked || recorded || key_env != env(), now < T) {
                        (true, _) => "refused",
                        (false, true) => "granted",
                        (false, false) => "newly expired",
                    };
                    let got = recheck(&rec, &env(), at(now));
                    assert_eq!(verdict(got), expected, "recheck, {case}");
                    let got = verify(&right, Some(&rec), &env(), at(now));
                    assert_eq!(verdict(got), expected, "verify, {case}");
                    let got = verify(&wrong, Some(&rec), &env(), at(now));
                    assert_eq!(verdict(got), "refused", "wrong secret, {case}");
                }
            }
        }
    }
    let unknown = verify(&right, None, &env(), at(T));
    assert_eq!(verdict(unknown), "refused", "unknown");
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

/// The rule of D28, but for the label: within the creator's scopes and lifetime, as its record
/// was made, expiring after now.
fn within(
    (creator_scopes, creator_expiry): (&BTreeSet<Scope>, Option<Timestamp>),
    scopes: &BTreeSet<Scope>,
    expires_at: Option<Timestamp>,
    now: Timestamp,
) -> bool {
    let within_lifetime = match (creator_expiry, expires_at) {
        (None, _) => true,
        (Some(_), None) => false,
        (Some(c), Some(e)) => e <= c,
    };
    let within_scope = !scopes.is_empty() && scopes.is_subset(creator_scopes);
    within_scope && within_lifetime && expires_at.is_none_or(|e| e > now)
}

/// Over random creators, requests and times: as the grid below, at any time and expiry, near each
/// other or far apart (TEN-5).
#[test]
fn no_created_key_exceeds_its_creator_at_any_time() {
    // SplitMix64, its seed fixed, so a failure replays.
    let mut state = 101_u64;
    let mut random = || {
        state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let z = (state ^ (state >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        let z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    };
    let sets = every_scope_set();
    let mut seen = [0; 2];
    for case in 0..20_000 {
        let now = random().cast_signed();
        // A step of up to 2 ms, or of any length.
        let mut step = || {
            let far = random();
            let len = if far & 1 == 0 {
                far >> 1
            } else {
                (far >> 1) % 3
            };
            len.cast_signed()
        };
        let creator_expiry = (step() & 3 != 0).then(|| at(now.saturating_add(step().max(1))));
        let base = creator_expiry.map_or(now, Timestamp::as_millis);
        let expires_at = match step() & 3 {
            0 => None,
            1 => Some(at(now.saturating_add(step()))),
            2 => Some(at(base.saturating_add(step()))),
            _ => Some(at(base.saturating_sub(step()))),
        };
        let mut set = || &sets[usize::try_from(random() % 8).unwrap()];
        let (creator_scopes, scopes) = (set(), set());
        let held = record(creator_scopes, creator_expiry);
        // At the last millisecond there is, no expiry is after now.
        let Ok(creator) = recheck(&held, &env(), at(now)) else {
            continue;
        };
        let allowed = may_create(Some(&creator), scopes, expires_at, None, at(now));
        let expected = within(
            (creator_scopes, creator_expiry),
            scopes,
            expires_at,
            at(now),
        );
        assert_eq!(
            allowed.is_ok(),
            expected,
            "case {case}: {creator:?} at {now}; asked {scopes:?} until {expires_at:?}: {allowed:?}"
        );
        seen[usize::from(expected)] += 1;
    }
    assert!(seen.iter().all(|n| *n > 1_000), "{seen:?}");
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
                        let allowed = may_create(Some(&creator), &scopes, expires_at, label, at(T));
                        let made = (&creator_scopes, creator_expiry);
                        let within = within(made, &scopes, expires_at, at(T));
                        assert_eq!(
                            allowed.is_ok(),
                            within && label_ok,
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

#[test]
fn a_key_has_expired_from_its_expiry_on_or_once_one_is_recorded() {
    let never = record(&BTreeSet::from([Scope::Read]), None);
    assert!(!never.expired(at(i64::MAX)));
    let ends = record(&BTreeSet::from([Scope::Read]), Some(at(T)));
    assert!(!ends.expired(at(T - 1)));
    assert!(ends.expired(at(T)));
    assert!(ends.expired(at(T + 1)));
    // D42: a recorded expiry holds whatever the clock does, a key that never expires included.
    for expires_at in [None, Some(at(T))] {
        let recorded = KeyRecord {
            expiry_recorded: true,
            ..record(&BTreeSet::from([Scope::Read]), expires_at)
        };
        assert!(recorded.expired(at(T - HOUR)), "{expires_at:?}");
    }
}

/// The embedding process, which no grant backs, creates as an unexpiring key with every scope
/// would (D121).
#[test]
fn the_embedder_creates_as_an_unexpiring_key_with_every_scope() {
    let every = grant(&BTreeSet::from(Scope::ALL), None);
    for scopes in every_scope_set() {
        for expires_at in [None, Some(at(T - 1)), Some(at(T)), Some(at(T + HOUR))] {
            for label in [None, Some("agent-7"), Some("a\nb")] {
                assert_eq!(
                    may_create(None, &scopes, expires_at, label, at(T)),
                    may_create(Some(&every), &scopes, expires_at, label, at(T)),
                    "{scopes:?} until {expires_at:?}, label {label:?}"
                );
            }
        }
    }
}
