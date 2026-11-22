// Not compiled: the forms only ci/no-event-clock-outside-ledger.sh sees inside ironflowx-core,
// where clippy cannot resolve the path. A renamed import, and `Self` in a second impl block:
use crate::SequencedTime as S;

pub fn aliased(at: crate::Timestamp) -> S {
    S::from_event_clock(at)
}

impl crate::SequencedTime {
    pub fn again(at: crate::Timestamp) -> Self {
        Self::from_event_clock(at)
    }
}
