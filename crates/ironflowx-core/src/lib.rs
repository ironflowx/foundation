//! Ironflow X core types: validated opaque IDs, the environment reference, attribution, scopes,
//! addresses, secrets, stored and event-clock time, the clock trait, the error kinds and the size
//! limits of what a caller sends.
//!
//! This crate is pure: no I/O, no async runtime, no ambient clock, no randomness.

// Purity (LED-15): forbid, so an inner #[allow] cannot switch these off.
#![forbid(unsafe_code)]
#![forbid(clippy::disallowed_methods, clippy::disallowed_types)]
#![forbid(clippy::print_stdout, clippy::print_stderr, clippy::dbg_macro)]

mod error;
#[cfg(feature = "__fault-points")]
mod fault;
mod id;
pub mod limits;
mod secret;
mod tenancy;
mod time;
mod who;

pub use error::{ErrorKind, ParseError};
#[cfg(feature = "__fault-points")]
#[doc(hidden)]
pub use fault::{FaultPoint, reach_fault_point, set_fault_hook, set_key_hook, tell_key};
pub use id::{EnvRef, EnvironmentId, EventId, IncarnationToken, KeyId, ProjectId, RunId, TenantId};
pub use secret::Secret;
pub use tenancy::{Address, KeyOrigin, Scope};
pub use time::{Clock, SequencedTime, Timestamp};
pub use who::{Emitter, SubmitAs};
