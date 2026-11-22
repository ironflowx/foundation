//! Ironflow X core types: validated opaque IDs, the environment reference, stored and
//! event-clock time, the clock trait and the error kinds.
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
mod time;

pub use error::{ErrorKind, ParseError};
#[cfg(feature = "__fault-points")]
#[doc(hidden)]
pub use fault::{FaultPoint, reach_fault_point, set_fault_hook};
pub use id::{EnvRef, EnvironmentId, EventId, IncarnationToken};
pub use time::{Clock, SequencedTime, Timestamp};
