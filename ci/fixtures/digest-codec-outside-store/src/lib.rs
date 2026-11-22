//! Lint fixture: a crate other than the control plane reads a key digest's bytes, and makes one
//! from bytes. `ci/must-reject.sh` requires clippy and `ci/no-digest-codec.sh` to refuse both.

use ironflowx_authz::KeyDigest;

pub fn exposes(digest: &KeyDigest) -> [u8; 32] {
    digest.to_stored()
}

pub fn forges(bytes: &[u8]) -> Option<KeyDigest> {
    KeyDigest::from_stored(bytes)
}
