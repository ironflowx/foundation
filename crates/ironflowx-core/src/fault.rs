//! The crash test's named fault points (#6, D23). The hook is process-wide, so a point inside open
//! needs no `Ledger` to reach it, and in core so any crate's path can reach it.

use std::sync::OnceLock;

/// A place in the commit or recovery path where the crash test kills the process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultPoint {
    /// The writer has decided a record and not yet appended it.
    BeforeAppend,
    /// The record is durable and its submitter not yet answered.
    AfterFsync,
    /// The submitter is answered and the record not yet folded into the read store.
    AfterAck,
    /// Inside the writer's read store transaction for one record: its rows written, the
    /// applied position not yet moved, nothing committed.
    MidFold,
    /// The same place in open's recovery, folding a record the read store lacks.
    DuringOpen,
}

impl FaultPoint {
    /// The name the crash test uses for this point.
    pub const fn name(self) -> &'static str {
        match self {
            Self::BeforeAppend => "before-append",
            Self::AfterFsync => "after-fsync",
            Self::AfterAck => "after-ack",
            Self::MidFold => "mid-fold",
            Self::DuringOpen => "during-open",
        }
    }
}

static HOOK: OnceLock<fn(FaultPoint)> = OnceLock::new();

/// Calls `hook` on the thread that reaches each point, before the point's work. The
/// first hook set stays.
pub fn set_fault_hook(hook: fn(FaultPoint)) {
    let _ = HOOK.set(hook);
}

/// Calls the hook, if one is set, at `point`.
pub fn reach_fault_point(point: FaultPoint) {
    if let Some(hook) = HOOK.get() {
        hook(point);
    }
}
