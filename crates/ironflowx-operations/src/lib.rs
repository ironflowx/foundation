//! The Ironflow X engine: one verified way in. Every call is admitted against the control
//! plane (its key parsed, looked up and verified) before its handler reaches the environment's
//! ledger or its key records.
//!
//! [`Engine::open`] opens a data directory, bootstrapping it on first open and returning the
//! bootstrap key's secret once.

#![forbid(unsafe_code)]

mod admission;
mod open;

use std::collections::BTreeSet;
use std::fs::File;
use std::sync::Arc;

use ironflowx_cell::{Ledger, LocalOperations};
use ironflowx_core::{Address, Clock, EnvRef, ErrorKind, Scope, Secret};
use ironflowx_proto::{
    Call, CreateKeyRequest, CreatedKey, EmitBatchRequest, EmitBatchResponse, EmitRequest,
    EmitResponse, Environment, Event, EventPage, GetEventRequest, KeyInfo, KeyOrigin, KeyStatus,
    ListEnvironmentsRequest, ListEventsRequest, OpError, Operation, Operations, Whoami,
    WhoamiRequest,
};
use ironflowx_store::{ControlPlane, EnvironmentRecord, InsertError, NewKey, StoreError};

pub use admission::Authorized;
pub use open::EngineError;

/// An open data directory: its control plane, its environment's ledger and the root's lock.
pub struct Engine {
    ledger: Ledger,
    control: Arc<ControlPlane>,
    env: EnvRef,
    /// The ledger's clock, which also decides key expiry.
    clock: Arc<dyn Clock + Send + Sync>,
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
        let a = self.admit(cx, Operation::Whoami).await?;
        let record = self.environment(&a).await?;
        Ok(Whoami {
            key_id: a.grant().key_id(),
            scopes: a.grant().scopes().clone(),
            environment: address(&record),
            expires_at: a.grant().expires_at(),
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
        let a = self.admit(cx, Operation::ListEnvironments).await?;
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
        let a = self.admit(cx, Operation::Emit).await?;
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
        let a = self.admit(cx, Operation::EmitBatch).await?;
        self.ledger(&a).emit_batch(cx, req).await
    }

    /// Reads one event of the key's environment by ID.
    ///
    /// # Errors
    ///
    /// As [`Engine::whoami`]; `NotFound` for an unknown ID.
    pub async fn get_event(&self, cx: &Call, req: GetEventRequest) -> Result<Event, OpError> {
        let a = self.admit(cx, Operation::GetEvent).await?;
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
        let a = self.admit(cx, Operation::ListEvents).await?;
        self.ledger(&a).list_events(cx, req).await
    }

    /// Creates a key in the calling key's environment, holding no scope the caller lacks and
    /// outliving it in no way; the answer holds the key's secret, returned only here.
    ///
    /// # Errors
    ///
    /// As [`Engine::whoami`]; `PermissionDenied` without `admin`; `Invalid` for a request
    /// beyond the caller, an expiry not after now, or a bad label; `Unauthenticated` if the
    /// caller expired before the key was stored.
    ///
    /// # Panics
    ///
    /// If the OS random source fails.
    pub async fn create_key(
        &self,
        cx: &Call,
        req: CreateKeyRequest,
    ) -> Result<CreatedKey, OpError> {
        let a = self.admit(cx, Operation::CreateKey).await?;
        let creator = a.grant().key_id();
        let scopes = if req.scopes.is_empty() {
            BTreeSet::from([Scope::Read])
        } else {
            req.scopes
        };
        let (id, key, digest) = open::new_key();
        let new = NewKey {
            id,
            digest,
            scopes,
            label: req.label,
            expires_at: req.expires_at,
        };
        let (keys, env, clock) = (self.keys(&a), self.env, self.clock.clone());
        let row = tokio::task::spawn_blocking(move || keys.insert_key(&env, new, creator, &*clock))
            .await
            .map_err(|_| unavailable())?
            .map_err(|e| match e {
                InsertError::Creator => admission::unauthenticated("invalid API key", None),
                InsertError::Refused(why) => OpError {
                    kind: ErrorKind::Invalid,
                    message: why.to_string(),
                    hint: None,
                },
                InsertError::Store(_) => unavailable(),
            })?;
        Ok(CreatedKey {
            key,
            info: KeyInfo {
                key_id: id,
                label: row.label,
                scopes: row.record.scopes,
                origin: KeyOrigin::Created,
                created_by: Some(creator),
                created_at: row.created_at,
                expires_at: row.record.expires_at,
                status: KeyStatus::Active,
            },
        })
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
        Ok(Err(_)) | Err(_) => Err(unavailable()),
    }
}

fn unavailable() -> OpError {
    OpError {
        kind: ErrorKind::Unavailable,
        message: "the control plane is unavailable".into(),
        hint: Some("retry at once".into()),
    }
}
