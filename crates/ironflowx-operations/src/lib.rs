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
use ironflowx_core::{
    Address, Clock, EnvRef, ErrorKind, IncarnationToken, Scope, Secret, Timestamp,
};
use ironflowx_proto::{
    Call, CreateKeyRequest, CreatedKey, EmitBatchRequest, EmitBatchResponse, EmitRequest,
    EmitResponse, Environment, Event, EventPage, GetEventRequest, KeyInfo, KeyPage, KeyStatus,
    ListEnvironmentsRequest, ListEventsRequest, ListKeysRequest, OpError, Operation, Operations,
    RevokeKeyRequest, Whoami, WhoamiRequest,
};
use ironflowx_store::{
    ControlPlane, EnvironmentRecord, KeyChangeError, KeyRow, NewKey, StoreError,
};

/// A page of keys when none is asked for, and the most one page holds: one page can show
/// every key the cap allows.
const KEY_PAGE: u32 = 100;
const MAX_KEY_PAGE: u32 = 1_000;
/// The most created keys of an environment that hold a slot at once (TEN-15).
const KEY_CAP: u32 = 1_000;

pub use admission::Authorized;
pub use ironflowx_authz::SeedState;
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
    /// The bootstrap key's secret, returned once: only by an open that bootstrapped without a
    /// seed.
    pub minted: Option<Secret>,
    /// Whether the seed the open was given still works: a host should warn of a revoked one.
    /// `None` when it was given none.
    pub seed: Option<SeedState>,
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
    /// `Unauthenticated` for a missing or invalid key, whether or not its expiry could be
    /// recorded; `Unavailable` if the control plane fails otherwise.
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
    /// beyond the caller, an expiry not after now, or a bad label; `ResourceExhausted` while
    /// 1,000 created keys hold a slot: not revoked, and no expiry recorded; `Unauthenticated` if
    /// the caller expired before the key was stored.
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
        let row = key_change(move || keys.insert_key(&env, new, creator, &*clock, KEY_CAP)).await?;
        let info = key_info(row, self.clock.now());
        Ok(CreatedKey { key, info })
    }

    /// A page of the calling key's environment's keys, newest first, never a secret.
    ///
    /// # Errors
    ///
    /// As [`Engine::whoami`]; `PermissionDenied` without `admin`; `Invalid` for a cursor that
    /// does not decode or names another incarnation.
    pub async fn list_keys(&self, cx: &Call, req: ListKeysRequest) -> Result<KeyPage, OpError> {
        let a = self.admit(cx, Operation::ListKeys).await?;
        let token = self.env.token();
        let before = req
            .cursor
            .map(|c| {
                decode_cursor(&c, token).ok_or_else(|| OpError {
                    kind: ErrorKind::Invalid,
                    message: "invalid cursor".into(),
                    hint: None,
                })
            })
            .transpose()?;
        let limit = req.limit.map_or(KEY_PAGE, |l| l.get().min(MAX_KEY_PAGE));
        let (keys, env) = (self.keys(&a), self.env);
        // One more than the page, so a full last page carries no cursor.
        let mut rows = blocking(move || keys.list_keys(&env, before, limit + 1)).await?;
        let limit = limit as usize;
        let next = (rows.len() > limit).then(|| {
            rows.truncate(limit);
            format!("{token}.{}", rows[limit - 1].seq)
        });
        let now = self.clock.now();
        Ok(KeyPage {
            keys: rows.into_iter().map(|row| key_info(row, now)).collect(),
            next,
        })
    }

    /// Revokes a key of the calling key's environment; the row stays (D35). Revoking a revoked
    /// key succeeds.
    ///
    /// # Errors
    ///
    /// As [`Engine::whoami`]; `PermissionDenied` without `admin`; `NotFound` "key not found"
    /// for an ID that does not parse or names no key here; `Unauthenticated` if the calling key
    /// was revoked or expired before the revoke's transaction.
    pub async fn revoke_key(&self, cx: &Call, req: RevokeKeyRequest) -> Result<(), OpError> {
        let a = self.admit(cx, Operation::RevokeKey).await?;
        let id = req.key_id.parse().map_err(|_| key_not_found())?;
        let by = a.grant().key_id();
        let (keys, env, clock) = (self.keys(&a), self.env, self.clock.clone());
        key_change(move || keys.revoke_key(&env, id, by, &*clock)).await
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

/// What is listed about a key, its status by the clock and its recorded expiry
/// (architecture.md, "Keys").
fn key_info(row: KeyRow, now: Timestamp) -> KeyInfo {
    let r = row.record;
    let status = if r.revoked_at.is_some() {
        KeyStatus::Revoked
    } else if r.expiry_recorded || r.expires_at.is_some_and(|t| now >= t) {
        KeyStatus::Expired
    } else {
        KeyStatus::Active
    };
    KeyInfo {
        key_id: r.id,
        label: row.label,
        scopes: r.scopes,
        origin: row.origin,
        created_by: row.created_by,
        created_at: row.created_at,
        expires_at: r.expires_at,
        status,
    }
}

/// A key cursor is `<incarnation token>.<last seq returned>`, as event cursors are: an
/// encoding, not a signature. Only the form this module writes decodes.
fn decode_cursor(cursor: &str, token: IncarnationToken) -> Option<u64> {
    let seq: u64 = cursor.rsplit_once('.')?.1.parse().ok()?;
    // Checks the token, and that there is no sign and no leading zero; a seq is an SQLite rowid.
    (cursor == format!("{token}.{seq}") && i64::try_from(seq).is_ok()).then_some(seq)
}

fn key_not_found() -> OpError {
    OpError {
        kind: ErrorKind::NotFound,
        message: "key not found".into(),
        hint: None,
    }
}

/// Runs a key change off the runtime and answers its refusal.
async fn key_change<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, KeyChangeError> + Send + 'static,
) -> Result<T, OpError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|_| unavailable())?
        .map_err(|e| match e {
            KeyChangeError::Caller => admission::unauthenticated("invalid API key", None),
            KeyChangeError::Refused(why) => OpError {
                kind: ErrorKind::Invalid,
                message: why.to_string(),
                hint: None,
            },
            KeyChangeError::Full => OpError {
                kind: ErrorKind::ResourceExhausted,
                message: "key limit reached".into(),
                hint: Some("revoke keys, expired ones first".into()),
            },
            KeyChangeError::NotFound => key_not_found(),
            KeyChangeError::Store(_) => unavailable(),
        })
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
