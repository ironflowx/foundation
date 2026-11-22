//! Lint fixture that ci/no-key-attribution.sh must pass: comments may name the constructor.

/// Keys are attributed with [`SubmitAs::key`] (D36).
// SubmitAs::key is confined to the credentials crate.
pub fn documented() {}
