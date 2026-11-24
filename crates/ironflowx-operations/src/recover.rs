//! The way back in for whoever holds the data directory (D34).

use std::path::Path;

use ironflowx_core::{Clock, Secret};
use ironflowx_store::{BootstrapState, ControlPlane, RecoverError};

use crate::open::new_admin_key;

/// Stores a fresh admin key in the data directory `root` and returns its secret, once: all three
/// scopes, no expiry, origin `Recovered`, labelled `recovered`, holding no slot. For a host, not
/// an operation: reaching the directory is the authority. It takes no engine lock, so it works
/// beside a running engine, which accepts the key on its next call.
///
/// Blocking. It waits a bounded time for a write of the engine's, as that does for this one.
///
/// # Errors
///
/// `NotReady` until an open has finished bootstrapping `root`: a missing or empty directory stays
/// so. `Store` if `control.db` is in another format or SQLite fails. No key is stored then.
///
/// # Panics
///
/// If the OS random source fails.
pub fn recover_admin_key(root: &Path, clock: impl Clock) -> Result<Secret, RecoverError> {
    let path = root.join("control.db");
    // Opening to write would create the file. Whether the bootstrap is marked is not decided
    // here, but in the transaction that inserts (TEN-14).
    match ControlPlane::bootstrap_state(&path)? {
        BootstrapState::NoFile | BootstrapState::Unbootstrapped => {
            return Err(RecoverError::NotReady);
        }
        BootstrapState::Unmarked { .. } | BootstrapState::Marked { .. } => {}
    }
    let (key, secret) = new_admin_key();
    ControlPlane::open(&path)?.insert_recovered(&key, &clock)?;
    Ok(secret)
}
