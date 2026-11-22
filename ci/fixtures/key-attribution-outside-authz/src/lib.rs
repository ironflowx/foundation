//! Lint fixture: a crate other than the credentials crate attributes a submission to a key.
//! `ci/must-reject.sh` requires clippy and `ci/no-key-attribution.sh` to refuse it.

use ironflowx_core::{KeyId, SubmitAs};

pub fn attributes_to_a_key(id: KeyId) -> SubmitAs {
    SubmitAs::key(id)
}
