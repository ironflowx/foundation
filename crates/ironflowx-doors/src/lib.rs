//! The doors an Ironflow X engine is called through. A door holds no rule: it decodes a request,
//! calls `Operations` once and sends the answer back (SRV-1).

#![forbid(unsafe_code)]

#[cfg(feature = "http")]
mod http;

#[cfg(feature = "http")]
pub use http::router;

use std::future::Future;
use std::sync::{Arc, Mutex};

use ironflowx_proto::Operations;

/// What a host that cannot serve calls yet waits for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitingOn {
    /// The engine, to open.
    Open,
    /// The control plane, to answer.
    ControlPlane,
    /// The ledger, which has failed.
    Ledger,
}

/// Whether an open engine can serve calls, asked afresh by each `/ready`.
pub trait Readiness: Send + Sync {
    fn ready(&self) -> impl Future<Output = Result<(), WaitingOn>> + Send;
}

/// Where a host puts the engine once it is open. The door admits calls only while open.
pub struct Slot<O: Operations, R: Readiness>(Mutex<State<O, R>>);

// Only the HTTP door reads what an open slot holds.
#[cfg_attr(not(feature = "http"), allow(dead_code))]
enum State<O, R> {
    Starting,
    Open(Arc<O>, Arc<R>),
    Draining,
    Closed,
}

impl<O: Operations, R: Readiness> Slot<O, R> {
    /// A slot whose engine is not open yet.
    pub const fn starting() -> Self {
        Self(Mutex::new(State::Starting))
    }

    /// Serves calls through `operations` from now on.
    ///
    /// # Panics
    ///
    /// If the slot has left Starting.
    pub fn open(&self, operations: Arc<O>, readiness: R) {
        let mut state = self.0.lock().expect("slot lock");
        assert!(matches!(*state, State::Starting), "a slot opens once");
        *state = State::Open(operations, Arc::new(readiness));
    }

    /// Refuses further admissions. Already admitted calls keep their references.
    pub fn drain(&self) {
        *self.0.lock().expect("slot lock") = State::Draining;
    }

    /// Marks the slot closed, before the host closes the engine.
    pub fn close(&self) {
        *self.0.lock().expect("slot lock") = State::Closed;
    }
}
