//! `KeyDigest`: the SHA-256 of a key's whole text, the only form of a key that is stored.

use std::fmt;

use sha2::{Digest as _, Sha256};
use subtle::ConstantTimeEq as _;

/// The SHA-256 of a key's text (D46). Its bytes are private: `ct_eq` is its only comparison,
/// and `to_stored` and `from_stored`, its storage codec, are its only exposure, which a lint
/// confines to the control plane (TEN-4). It has no `==`:
///
/// ```compile_fail,E0369
/// # use ironflowx_authz::KeyDigest;
/// fn same(a: &KeyDigest, b: &KeyDigest) -> bool { a == b }
/// ```
#[derive(Clone)]
pub struct KeyDigest([u8; 32]);

#[cfg(any(test, feature = "__count-ct-eq"))]
thread_local! {
    /// How many times this thread called `ct_eq`, so a test can show a comparison reaches it.
    pub(crate) static CT_EQ_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

impl KeyDigest {
    pub(crate) fn of(text: &str) -> Self {
        Self(Sha256::digest(text.as_bytes()).into())
    }

    /// Compares in constant time.
    pub fn ct_eq(&self, other: &Self) -> bool {
        #[cfg(any(test, feature = "__count-ct-eq"))]
        CT_EQ_CALLS.set(CT_EQ_CALLS.get() + 1);
        self.0.ct_eq(&other.0).into()
    }

    /// The bytes to store. Only the control plane calls this (TEN-4).
    pub const fn to_stored(&self) -> [u8; 32] {
        self.0
    }

    /// A digest read back from storage; `None` unless `bytes` is 32 long. Only the control
    /// plane calls this (TEN-4).
    pub fn from_stored(bytes: &[u8]) -> Option<Self> {
        bytes.try_into().ok().map(Self)
    }
}

/// Prints no byte of the digest.
impl fmt::Debug for KeyDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("KeyDigest(..)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_digest_is_sha256_of_the_whole_text() {
        // SHA-256("abc"), FIPS 180-2 appendix B.1.
        let abc = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        let hex: String = KeyDigest::of("abc")
            .0
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(hex, abc);
    }
}
