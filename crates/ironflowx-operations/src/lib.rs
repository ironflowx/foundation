//! The Ironflow X engine: one verified way in. Every call is admitted against the control
//! plane (its key parsed, looked up and verified) before its handler reaches the environment's
//! ledger or its key records.
//!
//! [`Engine::open`] opens a data directory, bootstrapping it on first open and returning the
//! bootstrap key's secret once.

#![forbid(unsafe_code)]

mod admission;
mod open;

use std::fs::File;
use std::sync::Arc;

use ironflowx_cell::{Ledger, LocalOperations};
use ironflowx_core::{Address, EnvRef, ErrorKind, Secret};
use ironflowx_proto::{
    Call, EmitBatchRequest, EmitBatchResponse, EmitRequest, EmitResponse, Environment, Event,
    EventPage, GetEventRequest, ListEnvironmentsRequest, ListEventsRequest, OpError, Operations,
    Whoami, WhoamiRequest,
};
use ironflowx_store::{ControlPlane, EnvironmentRecord, StoreError};

pub use admission::Authorized;
pub use open::EngineError;

/// An open data directory: its control plane, its environment's ledger and the root's lock.
pub struct Engine {
    ledger: Ledger,
    control: Arc<ControlPlane>,
    env: EnvRef,
    // Held until `close`, after the ledger closes.
    _lock: File,
}

/// What an open returns.
pub struct Opened {
    pub engine: Engine,
    /// The bootstrap key's secret, returned once: only by the open that bootstrapped.
    pub minted: Option<Secret>,
}

impl Engine {
    /// Closes the ledger, then releases the root.
    ///
    /// # Panics
    ///
    /// Outside a tokio runtime.
    pub async fn close(self) {
        self.ledger.close().await;
    }

    /// The calling key, as it sees itself. Any valid key may ask.
    ///
    /// # Errors
    ///
    /// `Unauthenticated` for a missing or invalid key; `Unavailable` if the control plane
    /// fails.
    pub async fn whoami(&self, cx: &Call, _req: WhoamiRequest) -> Result<Whoami, OpError> {
        let a = self.admit(cx).await?;
        let record = self.environment(&a).await?;
        Ok(Whoami {
            key_id: a.grant().key_id(),
            scopes: a.grant().scopes().clone(),
            environment: address(&record),
            expires_at: None,
        })
    }

    /// The environments the calling key reaches: only its own.
    ///
    /// # Errors
    ///
    /// As [`Engine::whoami`].
    pub async fn list_environments(
        &self,
        cx: &Call,
        _req: ListEnvironmentsRequest,
    ) -> Result<Vec<Environment>, OpError> {
        let a = self.admit(cx).await?;
        let record = self.environment(&a).await?;
        Ok(vec![Environment {
            env: record.env,
            address: address(&record),
            name: record.name,
            project_id: record.project_id,
            created_at: record.created_at,
        }])
    }

    /// Emits one event, attributed to the calling key.
    ///
    /// # Errors
    ///
    /// As [`Engine::whoami`], or as the ledger refuses the event.
    pub async fn emit(&self, cx: &Call, req: EmitRequest) -> Result<EmitResponse, OpError> {
        let a = self.admit(cx).await?;
        self.ledger(&a).emit(cx, req).await
    }

    /// Emits a batch, every event attributed to the calling key; one result per item, in
    /// order.
    ///
    /// # Errors
    ///
    /// As [`Engine::emit`].
    pub async fn emit_batch(
        &self,
        cx: &Call,
        req: EmitBatchRequest,
    ) -> Result<EmitBatchResponse, OpError> {
        let a = self.admit(cx).await?;
        self.ledger(&a).emit_batch(cx, req).await
    }

    /// Reads one event of the key's environment by ID.
    ///
    /// # Errors
    ///
    /// As [`Engine::whoami`]; `NotFound` for an unknown ID.
    pub async fn get_event(&self, cx: &Call, req: GetEventRequest) -> Result<Event, OpError> {
        let a = self.admit(cx).await?;
        self.ledger(&a).get_event(cx, req).await
    }

    /// Lists a page of the key's environment's events, newest first.
    ///
    /// # Errors
    ///
    /// As [`Engine::whoami`], or for a bad cursor.
    pub async fn list_events(
        &self,
        cx: &Call,
        req: ListEventsRequest,
    ) -> Result<EventPage, OpError> {
        let a = self.admit(cx).await?;
        self.ledger(&a).list_events(cx, req).await
    }

    /// The ledger, reached only with a proof of admission, submitting as its grant (TEN-1,
    /// TEN-6).
    fn ledger(&self, a: &Authorized) -> LocalOperations<'_> {
        LocalOperations::submitting_as(&self.ledger, a.grant().submit_as())
    }

    /// The key records, reached only with a proof of admission (TEN-1).
    fn keys(&self, _: &Authorized) -> Arc<ControlPlane> {
        self.control.clone()
    }

    async fn environment(&self, a: &Authorized) -> Result<EnvironmentRecord, OpError> {
        let keys = self.keys(a);
        let env = a.grant().env();
        blocking(move || keys.environment(&env)).await
    }
}

fn address(record: &EnvironmentRecord) -> Address {
    Address::new(&record.project_name, &record.name)
}

/// Runs a control-plane call off the runtime; any failure is `Unavailable` and changes nothing.
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, StoreError> + Send + 'static,
) -> Result<T, OpError> {
    match tokio::task::spawn_blocking(f).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(_)) | Err(_) => Err(OpError {
            kind: ErrorKind::Unavailable,
            message: "the control plane is unavailable".into(),
            hint: Some("retry at once".into()),
        }),
    }
}
