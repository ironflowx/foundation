/// A UTC instant in whole milliseconds since the Unix epoch: the one stored-time type.
///
/// `from_millis` and `as_millis` are its only conversions, so it has one encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp {
    millis: i64,
}

impl Timestamp {
    pub const fn from_millis(millis: i64) -> Self {
        Self { millis }
    }

    pub const fn as_millis(self) -> i64 {
        self.millis
    }
}

/// A reading of the ledger's event clock, which never goes back.
///
/// Pure code receives one and never reads a clock. It compares only with itself:
///
/// ```compile_fail,E0308
/// # use ironflowx_core::{SequencedTime, Timestamp};
/// fn mixed(event: SequencedTime, stored: Timestamp) -> bool { event < stored }
/// ```
///
/// ```compile_fail,E0308
/// # use ironflowx_core::SequencedTime;
/// fn mixed(event: SequencedTime) -> bool { event < std::time::SystemTime::now() }
/// ```
///
/// ```
/// # use ironflowx_core::SequencedTime;
/// fn ordered(a: SequencedTime, b: SequencedTime) -> bool { a < b }
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SequencedTime {
    at: Timestamp,
}

impl SequencedTime {
    /// Only the ledger stamps event time; a clippy `disallowed-methods` entry bans this call
    /// in every other crate.
    #[doc(hidden)]
    pub const fn from_event_clock(at: Timestamp) -> Self {
        Self { at }
    }

    /// The instant this reading names, to store.
    pub const fn timestamp(self) -> Timestamp {
        self.at
    }
}

/// A source of the current time. Core declares it and never calls it. `now` must not panic: it
/// runs on the ledger's writer and under the control plane's lock.
pub trait Clock {
    fn now(&self) -> Timestamp;
}
