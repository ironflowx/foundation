//! The doors an Ironflow X engine is called through. A door holds no rule: it decodes a request,
//! calls `Operations` once and sends the answer back (SRV-1).

#![forbid(unsafe_code)]

#[cfg(feature = "http")]
mod http;

#[cfg(feature = "http")]
pub use http::router;

use std::future::Future;
use std::sync::{Arc, OnceLock};

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

/// Where a host puts the engine once it is open; the doors answer for it until then.
pub struct Slot<O: Operations, R: Readiness>(OnceLock<(Arc<O>, R)>);

impl<O: Operations, R: Readiness> Slot<O, R> {
    /// A slot whose engine is not open yet.
    pub const fn starting() -> Self {
        Self(OnceLock::new())
    }

    /// Serves calls through `operations` from now on.
    ///
    /// # Panics
    ///
    /// If the slot is open already.
    pub fn open(&self, operations: Arc<O>, readiness: R) {
        let first = self.0.set((operations, readiness)).is_ok();
        assert!(first, "a slot opens once");
    }
}
