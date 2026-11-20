//! The two `Clock` adapters.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use ironflowx_core::{Clock, Timestamp};

/// The host's wall clock, so that production event time follows real time.
#[derive(Debug, Clone, Copy, Default)]
pub struct HostClock;

impl Clock for HostClock {
    fn now(&self) -> Timestamp {
        let since = SystemTime::now().duration_since(UNIX_EPOCH);
        // Before the epoch only when the host clock is wildly wrong; the event clock still
        // never goes back.
        Timestamp::from_millis(
            since.map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX)),
        )
    }
}

/// A clock a test sets and steps, backwards too. Clones share one reading.
#[derive(Debug, Clone)]
pub struct ManualClock(Arc<AtomicI64>);

impl ManualClock {
    pub fn new(at: Timestamp) -> Self {
        Self(Arc::new(AtomicI64::new(at.as_millis())))
    }

    pub fn set(&self, at: Timestamp) {
        self.0.store(at.as_millis(), Ordering::SeqCst);
    }

    /// Steps the clock by `millis`, which may be negative.
    pub fn step(&self, millis: i64) {
        self.0.fetch_add(millis, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(self.0.load(Ordering::SeqCst))
    }
}
