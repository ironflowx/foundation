//! Lint fixture: a crate other than the engine forges a grant from verify's public inputs and
//! takes its key attribution. `ci/must-reject.sh` requires clippy to refuse it.

use ironflowx_authz::{KeyRecord, mint, parse, verify};
use ironflowx_core::{EnvRef, KeyId, SubmitAs};

pub fn forges_a_key_attribution(id: KeyId, env: EnvRef) -> Option<SubmitAs> {
    let (secret, digest) = mint(id, [0; 32]);
    let parts = parse(&secret).ok()?;
    let record = KeyRecord {
        id,
        env,
        scopes: Default::default(),
        digest,
    };
    Some(verify(&parts, Some(&record), &env).ok()?.submit_as())
}
