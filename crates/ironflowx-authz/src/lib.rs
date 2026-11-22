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
use ironflowx_core::{EnvRef, KeyId, Scope, Secret, SubmitAs};

pub use digest::KeyDigest;

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
    // The decoder refuses padding, other alphabets and non-canonical trailing bits; 43
    // characters decode to exactly 32 bytes.
    URL_SAFE_NO_PAD.decode(secret).map_err(|_| Malformed)?;
    Ok(KeyParts {
        id,
        digest: KeyDigest::of(text),
    })
}

/// Makes a key from its ID and 256 random bits, which the caller draws.
pub fn mint(id: KeyId, secret: [u8; 32]) -> (Secret, KeyDigest) {
    let text = format!("{PREFIX}{id}_{}", URL_SAFE_NO_PAD.encode(secret));
    let digest = KeyDigest::of(&text);
    (Secret::new(text), digest)
}

/// What credentials need to know about a stored key.
#[derive(Debug, Clone)]
pub struct KeyRecord {
    pub id: KeyId,
    /// The environment ID and incarnation token current when the key was made.
    pub env: EnvRef,
    pub scopes: BTreeSet<Scope>,
    pub digest: KeyDigest,
}

/// What a verified key may do. Never stored; only [`verify`] makes one.
#[derive(Debug, Clone)]
pub struct Grant {
    key_id: KeyId,
    scopes: BTreeSet<Scope>,
    env: EnvRef,
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

    /// What a submission made with this key is attributed to: the key itself (TEN-6).
    pub const fn submit_as(&self) -> SubmitAs {
        SubmitAs::key(self.key_id)
    }
}

/// One answer for every key that is not valid: unknown, a wrong secret, or another
/// environment or incarnation, so no refusal tells which keys exist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused;

/// Decides whether `parts` is a valid key for `env`, given the record its ID looked up.
///
/// # Errors
///
/// [`Refused`], alike for every reason.
pub fn verify(
    parts: &KeyParts,
    record: Option<&KeyRecord>,
    env: &EnvRef,
) -> Result<Grant, Refused> {
    let record = record.ok_or(Refused)?;
    // The digest comparison runs whatever the other checks find.
    let same_secret = record.digest.ct_eq(&parts.digest);
    if same_secret && record.id == parts.id && record.env == *env {
        Ok(Grant {
            key_id: record.id,
            scopes: record.scopes.clone(),
            env: record.env,
        })
    } else {
        Err(Refused)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(n: u8) -> EnvRef {
        format!("019a9f3e-7c2b-7d41-8a3e-0000000000{n:02x}.{:032x}", 7)
            .parse()
            .unwrap()
    }

    /// `verify` compares digests through `ct_eq` once on every record it is given, whether or
    /// not the ID and environment match (TEN-4).
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
        ] {
            let before = digest::CT_EQ_CALLS.get();
            assert_eq!(
                verify(&parts, Some(&record), &env(1)).is_ok(),
                granted,
                "{case}"
            );
            assert_eq!(digest::CT_EQ_CALLS.get() - before, 1, "{case}");
        }
    }
}
