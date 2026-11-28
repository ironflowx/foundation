//! Lint fixture: a crate other than ironflowx-execution makes the engine's events, called by
//! name, through an alias and as a function value. `ci/must-reject.sh` requires clippy to refuse
//! each (RUN-14).

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
