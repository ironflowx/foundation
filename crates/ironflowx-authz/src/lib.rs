//! Ironflow X credentials: the key format, its digest, and whether a presented key is valid
//! for an environment.
//!
//! Pure: no I/O, no clock, no randomness. The engine passes in the random secret and the key
//! record it read.

// Purity (LED-15): forbid, so an inner #[allow] cannot switch these off.
#![forbid(unsafe_code)]
#![forbid(clippy::disallowed_methods, clippy::disallowed_types)]
#![forbid(clippy::print_stdout, clippy::print_stderr, clippy::dbg_macro)]

mod digest;

use std::collections::BTreeSet;
use std::fmt;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ironflowx_core::{EnvRef, KeyId, KeyOrigin, Scope, Secret, SubmitAs, Timestamp};

pub use digest::KeyDigest;

/// How many times this thread called [`KeyDigest::ct_eq`].
#[cfg(feature = "__count-ct-eq")]
#[doc(hidden)]
pub fn ct_eq_calls() -> usize {
    digest::CT_EQ_CALLS.get()
}

const PREFIX: &str = "ixk_";
/// `ixk_` + a 36-character key ID + `_` + 43 base64url characters (256 bits, no padding).
const KEY_LEN: usize = 84;
const ID_END: usize = PREFIX.len() + 36;

/// A key, parsed: its ID and the digest of its whole text.
#[derive(Debug, Clone)]
pub struct KeyParts {
    pub id: KeyId,
    pub digest: KeyDigest,
}

/// A key that is not in the key format. Its message names the format, never the key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Malformed;

impl fmt::Display for Malformed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(
            "a key is ixk_<key-id>_<secret>: 84 characters, a lowercase UUIDv7 key ID and 43 \
             base64url characters",
        )
    }
}

impl std::error::Error for Malformed {}

/// Parses a key by position, since the secret may hold `_` and `-` (D46).
///
/// # Errors
///
/// If `raw` is not `ixk_<key-id>_<43 base64url characters>`, 84 characters in all.
pub fn parse(raw: &Secret) -> Result<KeyParts, Malformed> {
    let text = raw.expose();
    // ASCII first, so the slices below fall on character boundaries.
    if text.len() != KEY_LEN || !text.is_ascii() {
        return Err(Malformed);
    }
    let (head, rest) = text.split_at(ID_END);
    let id = head.strip_prefix(PREFIX).ok_or(Malformed)?;
    let secret = rest.strip_prefix('_').ok_or(Malformed)?;
    let id = id.parse().map_err(|_| Malformed)?;
    // Checked without decoding, so no copy of the secret is made (TEN-3): base64url characters,
    // the last with its 2 spare bits zero, since 43 characters hold 258 bits.
    let base64url = |b: u8| b.is_ascii_alphanumeric() || b == b'-' || b == b'_';
    let canonical = secret.ends_with(|c| "AEIMQUYcgkosw048".contains(c));
    if !(secret.bytes().all(base64url) && canonical) {
        return Err(Malformed);
    }
    Ok(KeyParts {
        id,
        digest: KeyDigest::of(text),
    })
}

/// Makes a key from its ID and 256 random bits, which the caller draws.
pub fn mint(id: KeyId, secret: [u8; 32]) -> (Secret, KeyDigest) {
    // Encoded in place, into a block that never grows, so no copy of the secret is left (TEN-3).
    let mut text = String::with_capacity(KEY_LEN);
    text.push_str(&format!("{PREFIX}{id}_"));
    URL_SAFE_NO_PAD.encode_string(secret, &mut text);
    let digest = KeyDigest::of(&text);
    (Secret::new(text), digest)
}

/// The most bytes a key's label holds.
const MAX_LABEL_BYTES: usize = 128;

/// What credentials need to know about a stored key.
#[derive(Debug, Clone)]
pub struct KeyRecord {
    pub id: KeyId,
    /// The environment ID and incarnation token current when the key was made.
    pub env: EnvRef,
    pub scopes: BTreeSet<Scope>,
    pub digest: KeyDigest,
    /// `None`: the key never expires.
    pub expires_at: Option<Timestamp>,
    /// The key was refused as expired, and stays expired whatever the clock does (D42).
    pub expiry_recorded: bool,
    /// `Some`: the key was revoked then, and is refused from then on.
    pub revoked_at: Option<Timestamp>,
}

/// What a verified key may do. Never stored; only [`verify`] and [`recheck`] make one.
#[derive(Debug, Clone)]
pub struct Grant {
    key_id: KeyId,
    scopes: BTreeSet<Scope>,
    env: EnvRef,
    expires_at: Option<Timestamp>,
}

impl Grant {
    pub const fn key_id(&self) -> KeyId {
        self.key_id
    }

    pub const fn scopes(&self) -> &BTreeSet<Scope> {
        &self.scopes
    }

    pub const fn env(&self) -> EnvRef {
        self.env
    }

    /// `None`: the key never expires.
    pub const fn expires_at(&self) -> Option<Timestamp> {
        self.expires_at
    }

    /// What a submission made with this key is attributed to: the key itself (TEN-6).
    pub const fn submit_as(&self) -> SubmitAs {
        SubmitAs::key(self.key_id)
    }
}

/// One answer for every key that is not valid: unknown, a wrong secret, revoked, expired, or
/// another environment or incarnation, so no refusal tells which keys exist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused {
    newly_expired: bool,
}

impl Refused {
    const PLAIN: Self = Self {
        newly_expired: false,
    };

    /// True when the key is refused only for having reached an expiry not yet recorded, its
    /// secret matching if it was presented: the caller records the expiry (D42). It never
    /// shapes the answer.
    pub const fn newly_expired(&self) -> bool {
        self.newly_expired
    }
}

/// Decides whether `parts` is a valid key for `env` at `now`, given the record its ID looked
/// up.
///
/// # Errors
///
/// [`Refused`]: one answer for every reason.
pub fn verify(
    parts: &KeyParts,
    record: Option<&KeyRecord>,
    env: &EnvRef,
    now: Timestamp,
) -> Result<Grant, Refused> {
    let record = record.ok_or(Refused::PLAIN)?;
    // The digest comparison runs whatever the other checks find.
    let same_secret = record.digest.ct_eq(&parts.digest);
    if same_secret && record.id == parts.id {
        recheck(record, env, now)
    } else {
        Err(Refused::PLAIN)
    }
}

/// Decides whether the key `record` describes is still valid for `env` at `now`, without its
/// secret: a key change re-reads its creator's record inside its transaction and asks again.
///
/// # Errors
///
/// [`Refused`] for another environment or incarnation, a revoked key, a recorded expiry, or
/// from `now >= expires_at`.
pub fn recheck(record: &KeyRecord, env: &EnvRef, now: Timestamp) -> Result<Grant, Refused> {
    if record.env != *env || record.revoked_at.is_some() || record.expiry_recorded {
        return Err(Refused::PLAIN);
    }
    if record.expires_at.is_some_and(|t| now >= t) {
        return Err(Refused {
            newly_expired: true,
        });
    }
    Ok(Grant {
        key_id: record.id,
        scopes: record.scopes.clone(),
        env: record.env,
        expires_at: record.expires_at,
    })
}

/// Whether the seed a host supplied still works.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeedState {
    /// It is the one unrevoked seed.
    Current,
    /// It was revoked, by an admin or by a later seed, and stays revoked (D33).
    Revoked,
}

/// The seed's key ID is already a key, and not this seed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SeedConflict;

impl fmt::Display for SeedConflict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("the seed's key ID is already another key")
    }
}

impl std::error::Error for SeedConflict {}

/// Decides what the stored key `record`, of origin `origin`, found under `seed`'s key ID, makes
/// of `seed` in `env`.
///
/// # Errors
///
/// [`SeedConflict`] if the key has another digest or environment, or is not a seed.
pub fn verify_seed(
    seed: &KeyParts,
    record: &KeyRecord,
    origin: KeyOrigin,
    env: &EnvRef,
) -> Result<SeedState, SeedConflict> {
    // The digest comparison runs whatever the other checks find.
    let same_secret = record.digest.ct_eq(&seed.digest);
    let same_key = record.id == seed.id && record.env == *env && origin == KeyOrigin::Seed;
    if !(same_secret && same_key) {
        return Err(SeedConflict);
    }
    Ok(if record.revoked_at.is_some() {
        SeedState::Revoked
    } else {
        SeedState::Current
    })
}

/// A grant lacks the scope an operation requires (D31).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MissingScope(Scope);

impl MissingScope {
    pub const fn scope(self) -> Scope {
        self.0
    }
}

/// Admits `grant` to an operation that requires `required`; `None` admits any valid key. It
/// takes the scope, not the operation, so credentials stay free of the interface (D37).
///
/// # Errors
///
/// [`MissingScope`], naming the scope, if the grant does not hold it.
pub fn admit(grant: &Grant, required: Option<Scope>) -> Result<(), MissingScope> {
    match required {
        Some(scope) if !grant.scopes.contains(&scope) => Err(MissingScope(scope)),
        _ => Ok(()),
    }
}

/// Why a key may not be created as asked. Its message is not contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CreateRefusal(&'static str);

impl fmt::Display for CreateRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for CreateRefusal {}

/// Decides whether `creator` may create a key holding `scopes` until `expires_at`, labelled
/// `label`, at `now`. The new key holds no scope its creator lacks and outlives it in no way
/// (D28, TEN-5); its expiry is after now; its label is 0–128 bytes with no control character.
///
/// # Errors
///
/// [`CreateRefusal`], saying which rule the request breaks.
pub fn may_create(
    creator: &Grant,
    scopes: &BTreeSet<Scope>,
    expires_at: Option<Timestamp>,
    label: Option<&str>,
    now: Timestamp,
) -> Result<(), CreateRefusal> {
    let refuse = |why| Err(CreateRefusal(why));
    if scopes.is_empty() {
        return refuse("a key holds at least one scope");
    }
    if !scopes.is_subset(&creator.scopes) {
        return refuse("a key cannot hold a scope its creator lacks");
    }
    match (creator.expires_at, expires_at) {
        (_, Some(t)) if t <= now => return refuse("the expiry must be after now"),
        (Some(_), None) => return refuse("a key cannot outlive its creator: give it an expiry"),
        (Some(c), Some(t)) if t > c => return refuse("a key cannot expire after its creator"),
        _ => {}
    }
    if label.is_some_and(|l| l.len() > MAX_LABEL_BYTES || l.chars().any(char::is_control)) {
        return refuse("a label is at most 128 bytes, with no control character");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(n: u8) -> EnvRef {
        format!("019a9f3e-7c2b-7d41-8a3e-0000000000{n:02x}.{:032x}", 7)
            .parse()
            .unwrap()
    }

    /// `verify` compares digests through `ct_eq` once on every record it is given, whatever its
    /// ID, environment or state (TEN-4).
    #[test]
    fn verify_reaches_ct_eq_for_every_record() {
        let id: KeyId = "019a9f3e-7c2b-7d41-8a3e-5f6b7c8d9e0f".parse().unwrap();
        let other: KeyId = "019a9f3e-7c2b-7d41-8a3e-5f6b7c8d9e10".parse().unwrap();
        let parts = parse(&mint(id, [7; 32]).0).unwrap();
        let record = |id, bytes, env| KeyRecord {
            id,
            env,
            scopes: BTreeSet::new(),
            digest: mint(id, bytes).1,
            expires_at: None,
            expiry_recorded: false,
            revoked_at: None,
        };
        for (case, record, granted) in [
            ("matching", record(id, [7; 32], env(1)), true),
            ("wrong secret", record(id, [8; 32], env(1)), false),
            ("another environment", record(id, [7; 32], env(2)), false),
            (
                "another key's record, same digest",
                KeyRecord {
                    id: other,
                    ..record(id, [7; 32], env(1))
                },
                false,
            ),
            (
                "revoked",
                KeyRecord {
                    revoked_at: Some(Timestamp::from_millis(0)),
                    ..record(id, [7; 32], env(1))
                },
                false,
            ),
            (
                "expired",
                KeyRecord {
                    expires_at: Some(Timestamp::from_millis(0)),
                    ..record(id, [7; 32], env(1))
                },
                false,
            ),
            (
                "recorded as expired",
                KeyRecord {
                    expiry_recorded: true,
                    ..record(id, [7; 32], env(1))
                },
                false,
            ),
        ] {
            let before = digest::CT_EQ_CALLS.get();
            assert_eq!(
                verify(&parts, Some(&record), &env(1), Timestamp::from_millis(0)).is_ok(),
                granted,
                "{case}"
            );
            assert_eq!(digest::CT_EQ_CALLS.get() - before, 1, "{case}");
        }
    }

    /// `verify_seed` compares digests through `ct_eq` once on every record it is given, whatever
    /// its origin, environment or state, and only a seed with that digest is the host's (TEN-4).
    #[test]
    fn verify_seed_reaches_ct_eq_for_every_record() {
        use KeyOrigin::{Created, Minted, Recovered, Seed};
        let id: KeyId = "019a9f3e-7c2b-7d41-8a3e-5f6b7c8d9e0f".parse().unwrap();
        let other: KeyId = "019a9f3e-7c2b-7d41-8a3e-5f6b7c8d9e10".parse().unwrap();
        let seed = parse(&mint(id, [7; 32]).0).unwrap();
        for (record_id, matches_id) in [(id, true), (other, false)] {
            for (digest, same) in [(seed.digest.clone(), true), (mint(id, [8; 32]).1, false)] {
                for (key_env, own) in [(env(1), true), (env(2), false)] {
                    for origin in [Seed, Minted, Recovered, Created] {
                        for revoked_at in [None, Some(Timestamp::from_millis(5))] {
                            let record = KeyRecord {
                                id: record_id,
                                env: key_env,
                                scopes: BTreeSet::new(),
                                digest: digest.clone(),
                                expires_at: None,
                                expiry_recorded: false,
                                revoked_at,
                            };
                            let hosts = matches_id && same && own && origin == Seed;
                            let expected = match (hosts, revoked_at) {
                                (false, _) => Err(SeedConflict),
                                (true, None) => Ok(SeedState::Current),
                                (true, Some(_)) => Ok(SeedState::Revoked),
                            };
                            let before = digest::CT_EQ_CALLS.get();
                            let got = verify_seed(&seed, &record, origin, &env(1));
                            assert_eq!(got, expected, "{origin:?}: {record:?}");
                            assert_eq!(digest::CT_EQ_CALLS.get() - before, 1, "{record:?}");
                        }
                    }
                }
            }
        }
    }
}
