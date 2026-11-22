//! Lint fixture: a crate other than the engine forges a grant from verify's public inputs and
//! takes its key attribution. `ci/must-reject.sh` requires clippy to refuse it.

use ironflowx_authz::{KeyRecord, mint, parse, verify};
use ironflowx_core::{EnvRef, KeyId, SubmitAs, Timestamp};

pub fn forges_a_key_attribution(id: KeyId, env: EnvRef, now: Timestamp) -> Option<SubmitAs> {
    let (secret, digest) = mint(id, [0; 32]);
    let parts = parse(&secret).ok()?;
    let record = KeyRecord {
        id,
        env,
        scopes: Default::default(),
        digest,
        expires_at: None,
        revoked_at: None,
    };
    Some(verify(&parts, Some(&record), &env, now).ok()?.submit_as())
}
