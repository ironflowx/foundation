//! Lint fixture: a crate other than ironflowx-cell stamps event time. `ci/must-reject.sh`
//! requires clippy to refuse it.

use ironflowx_core::{SequencedTime, Timestamp};

pub fn stamps_event_time() -> SequencedTime {
    SequencedTime::from_event_clock(Timestamp::from_millis(0))
}
