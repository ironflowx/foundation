//! The Ironflow X ledger: one environment's durable commit log and read store, linked into the
//! embedder's process.
//!
//! [`Ledger::open`] opens one environment in a directory. A submission is acknowledged once its
//! record is fsynced, and a read sees every write acknowledged before it arrived.
//! [`LocalOperations`] puts the `Operations` interface over one ledger. This is the only crate
//! that touches disk, the clock and randomness.

#![forbid(unsafe_code)]

mod clock;
mod ledger;
mod ops;
mod record;
mod store;
#[cfg(test)]
mod tests;

pub use clock::{HostClock, ManualClock};
#[cfg(feature = "__fault-points")]
#[doc(hidden)]
pub use ledger::point::{FaultPoint, set_fault_hook};
pub use ledger::{Committed, Ledger, OpenError, ReadError, SubmitError, new_env_ref};
pub use ops::LocalOperations;
pub use store::{ReadStore, StoreError};
