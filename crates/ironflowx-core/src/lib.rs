//! Ironflow X core types: validated opaque IDs, the environment reference, stored and
//! event-clock time, the clock trait and the error kinds.
//!
//! This crate is pure: no I/O, no async runtime, no ambient clock, no randomness.

mod error;
mod id;
mod time;

pub use error::{ErrorKind, ParseError};
pub use id::{EnvRef, EnvironmentId, EventId, IncarnationToken};
pub use time::{Clock, SequencedTime, Timestamp};
