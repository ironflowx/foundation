//! Fixture: a crate other than ironflowx-execution makes the engine's events by name, alias and
//! function value, which clippy must refuse, and in a doctest and behind an off feature, which
//! ci/no-engine-event.sh must (RUN-14).

use ironflowx_events::engine_event as make;
use ironflowx_events::{EngineEmit, EngineEvent};

pub fn by_name(event: EngineEvent) -> EngineEmit {
    ironflowx_events::engine_event(event)
}

pub fn by_alias(event: EngineEvent) -> EngineEmit {
    make(event)
}

pub fn as_value(events: Vec<EngineEvent>) -> Vec<EngineEmit> {
    events.into_iter().map(ironflowx_events::engine_event).collect()
}

/// Named in a doctest, which clippy does not lint:
///
/// ```ignore
/// let _ = ironflowx_events::engine_event;
/// ```
pub fn in_a_doctest() {}

#[cfg(feature = "off")]
pub fn behind_a_feature(event: EngineEvent) -> EngineEmit {
    ironflowx_events::engine_event(event)
}
