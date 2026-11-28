//! The Ironflow X engine: one verified way in. Every call is admitted against the control
//! plane (its key parsed, looked up and verified) before its handler reaches the environment's
//! ledger or its key records.
//!
//! [`Engine::open`] opens a data directory, bootstrapping it on first open: the bootstrap key is
//! the seed the host supplies, or a minted one, whose secret it returns once.

#![forbid(unsafe_code)]

mod admission;
mod events;
mod open;
mod recover;

use std::collections::BTreeSet;
use std::fs::File;
use std::sync::Arc;

use ironflowx_cell::Ledger;
use ironflowx_core::{
    Address, Clock, Emitter, EnvRef, ErrorKind, IncarnationToken, ParseError, Scope, Secret,
    SubmitAs, Timestamp,
};
use ironflowx_events::{ItemOutcome, Submission};
use ironflowx_proto::{
    Call, CreateKeyRequest, CreatedKey, EmitBatchRequest, EmitBatchResponse, EmitRequest,
    EmitResponse, Environment, Event, EventPage, GetEventRequest, KeyInfo, KeyPage, KeyStatus,
    ListEnvironmentsRequest, ListEventsRequest, ListKeysRequest, OpError, Operation, Operations,
    RevokeKeyRequest, Whoami, WhoamiRequest,
};
use ironflowx_store::{ControlPlane, EnvironmentRecord, KeyChangeError, KeyRow, NewKey};

/// A page of keys when none is asked for, and the most one page holds: one page can show
/// every key the cap allows.
const KEY_PAGE: u32 = 100;
const MAX_KEY_PAGE: u32 = 1_000;
/// The most created keys of an environment that hold a slot at once (TEN-15).
const KEY_CAP: u32 = 1_000;

use admission::{Authorized, EmbedderGrant};
pub use events::check_emit;
pub use ironflowx_authz::SeedState;
pub use ironflowx_cell::{Fault, HostClock, LedgerState, OpenError};
pub use ironflowx_store::{RecoverError, StoreError};
pub use open::{EngineError, generate_seed, is_bootstrapped, key_id};
pub use recover::recover_admin_key;

/// An open data directory: its control plane, its environment's ledger and the root's lock.
///
/// Its operations, `open`, `health` and `close` need a tokio runtime: outside one, they panic at
/// the first step that runs on it, if they are not refused before. Dropping it
/// without `close` blocks until the emit in hand is done, ends queued ones `Unavailable` and
/// releases the root; async code calls `close`.
pub struct Engine {
    ledger: Ledger,
    control: Arc<ControlPlane>,
    env: EnvRef,
    /// The clock the engine was opened with, which decides key expiry.
    clock: Arc<dyn Clock + Send + Sync>,
    // Held until `close`, after the ledger closes.
    _lock: Arc<File>,
}

/// What an open returns.
pub struct Opened {
    pub engine: Engine,
    /// The bootstrap key's secret, returned once: only by an open without a seed that
    /// bootstrapped, or that replaced a key whose secret was lost.
    pub minted: Option<Secret>,
    /// This open finished an earlier one's bootstrap and revoked the key that one minted, whose
    /// secret no open returned.
    pub lost_secret: bool,
    /// Whether the seed the open was given still works: a host should warn of a revoked one.
    /// `None` when it was given none.
    pub seed: Option<SeedState>,
}

/// How an open engine is: each part alone, as it was when [`Engine::health`] read it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Health {
    pub control_plane: ControlPlaneState,
    pub ledger: LedgerState,
}

/// Whether the control plane answered a read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlPlaneState {
    Ok,
    /// The read failed. The engine stays open, and a later read may succeed, unless a call has
    /// panicked under the control plane's lock: then none does until the engine is reopened.
    Failed,
}

impl Engine {
    /// Reads the control plane once, then whether the ledger has failed, for the host's readiness.
    /// Not an operation: it takes no key, and reads no key record and no event (TEN-1).
    ///
    /// It waits for the control plane's lock, which a key change waiting for another process's
    /// write lock holds for seconds. It has no deadline: a host that needs one sets it.
    ///
    /// # Panics
    ///
    /// Outside a tokio runtime.
    pub async fn health(&self) -> Health {
        let (control, env) = (self.control.clone(), self.env);
        let read = blocking(move || control.environment(&env)).await;
        Health {
            control_plane: match read {
                Ok(_) => ControlPlaneState::Ok,
                Err(_) => ControlPlaneState::Failed,
            },
            ledger: self.ledger.state(),
        }
    }

    /// Resolves once, when the ledger fails, with a call in flight or none; at once if it has
    /// failed already. The engine stays open: the host closes it and opens it again. Not an
    /// operation: it takes no key (TEN-1).
    ///
    /// A close is no fault. What is pending when [`Engine::close`] begins resolves only if the
    /// ledger fails before `close` returns, as by the fold it finishes; after that it stays pending
    /// for good, so a host stops waiting for it once it has closed the engine. The same for a drop.
    pub fn faulted(&self) -> impl Future<Output = Fault> + Send + 'static + use<> {
        self.ledger.faulted()
    }

    /// Closes the ledger, then releases the root. What [`Engine::faulted`] gave stays pending,
    /// unless the ledger has failed or does before this returns.
    ///
    /// # Panics
    ///
    /// Outside a tokio runtime, or on one that has shut down: the engine is dropped then, its
    /// ledger's queue already ended.
    pub async fn close(self) {
        self.ledger.close().await;
    }

    /// Closes as [`Self::close`] does and returns the ledger state at the start of close.
    ///
    /// The ledger orders this start with fault publication. A later fault still resolves
    /// [`Self::faulted`] while the engine is closing.
    ///
    /// # Panics
    ///
    /// As [`Self::close`].
    pub async fn close_with_state(self) -> LedgerState {
        self.ledger.close_with_state().await
    }

    /// A call for the embedding process itself, attributed `embedder:<name>`: admitted as admin
    /// by this engine, and by a later one on the same directory, with no key, so an embedder
    /// stores no secret to stay in across restarts (D121). It reaches no other directory or
    /// incarnation, and no door can carry it: the wire has no field for it.
    ///
    /// # Errors
    ///
    /// If `name` is empty, over [`limits::MAX_EMBEDDER_NAME_BYTES`] bytes or holds a control,
    /// format or line or paragraph separator character.
    ///
    /// [`limits::MAX_EMBEDDER_NAME_BYTES`]: ironflowx_core::limits::MAX_EMBEDDER_NAME_BYTES
    pub fn embedder_call(&self, name: &str) -> Result<Call, ParseError> {
        let by = SubmitAs::embedder(name)?;
        Ok(Call::with_grant(EmbedderGrant { by, env: self.env }))
    }

    /// The ledger, reached only with a proof of admission (TEN-1).
    fn ledger(&self, _: &Authorized) -> &Ledger {
        &self.ledger
    }

    /// The engine's one way to submit: attributed to the admitted call's key or embedder (TEN-6).
    /// The hint of an `Unknown` is `if_unknown`.
    async fn submit(
        &self,
        a: &Authorized,
        sub: Submission,
        if_unknown: &str,
    ) -> Result<Vec<ItemOutcome>, OpError> {
        let by = a.submit_as();
        let committed = self.ledger(a).submit(sub, by).await;
        Ok(committed
            .map_err(|e| events::submit_error(e, if_unknown))?
            .outcome)
    }

    /// The key records, reached only with a proof of admission (TEN-1).
    fn keys(&self, _: &Authorized) -> Arc<ControlPlane> {
        self.control.clone()
    }

    async fn environment(&self, a: &Authorized) -> Result<EnvironmentRecord, OpError> {
        let keys = self.keys(a);
        let env = a.env();
        blocking(move || keys.environment(&env)).await
    }
}

impl Operations for Engine {
    /// The calling key, as it sees itself. Any valid key may ask, and an embedder, which holds
    /// every scope, no key and no expiry.
    ///
    /// # Errors
    ///
    /// `Unauthenticated` for a missing or invalid key, whether or not its expiry could be
    /// recorded; `Unavailable` if the control plane fails otherwise.
    async fn whoami(&self, cx: &Call, _req: WhoamiRequest) -> Result<Whoami, OpError> {
        let a = self.admit(cx, Operation::Whoami).await?;
        let record = self.environment(&a).await?;
        Ok(Whoami {
            key_id: a.key_id(),
            scopes: a.scopes(),
            environment: address(&record),
            expires_at: a.expires_at(),
        })
    }

    /// The environments the calling key reaches: only its own.
    ///
    /// # Errors
    ///
    /// As [`Engine::whoami`]; `PermissionDenied` without `read`.
    async fn list_environments(
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
    /// As [`Engine::whoami`]; `PermissionDenied` without `write`; as the ledger refuses the event;
    /// `Unknown` if the append's outcome is unknown; `Unavailable` if the ledger failed.
    async fn emit(&self, cx: &Call, req: EmitRequest) -> Result<EmitResponse, OpError> {
        let a = self.admit(cx, Operation::Emit).await?;
        let if_unknown = events::if_unknown(&req);
        let sub = Submission::Emit(events::emit(req));
        let outcomes = self.submit(&a, sub, if_unknown).await?;
        let [outcome] = <[_; 1]>::try_from(outcomes).expect("one outcome per emit");
        events::response(outcome)
    }

    /// Emits a batch, every event attributed to the calling key; one result per item, in
    /// order.
    ///
    /// # Errors
    ///
    /// As [`Engine::emit`].
    async fn emit_batch(
        &self,
        cx: &Call,
        req: EmitBatchRequest,
    ) -> Result<EmitBatchResponse, OpError> {
        let a = self.admit(cx, Operation::EmitBatch).await?;
        let sub = Submission::Batch(req.items.into_iter().map(events::emit).collect());
        let outcomes = self.submit(&a, sub, events::BATCH_IF_UNKNOWN).await?;
        Ok(EmitBatchResponse {
            items: outcomes.into_iter().map(events::response).collect(),
        })
    }

    /// Reads one event of the key's environment by ID.
    ///
    /// # Errors
    ///
    /// As [`Engine::whoami`]; `PermissionDenied` without `read`; `NotFound` for an unknown ID;
    /// `Unavailable` if the ledger failed.
    async fn get_event(&self, cx: &Call, req: GetEventRequest) -> Result<Event, OpError> {
        let a = self.admit(cx, Operation::GetEvent).await?;
        self.ledger(&a)
            .get_event(&req.event_id)
            .await
            .map(events::event)
            .map_err(events::read_error)
    }

    /// Lists a page of the key's environment's events, newest first.
    ///
    /// # Errors
    ///
    /// As [`Engine::whoami`]; `PermissionDenied` without `read`; `Invalid` for a bad cursor;
    /// `Unavailable` if the ledger failed.
    async fn list_events(&self, cx: &Call, req: ListEventsRequest) -> Result<EventPage, OpError> {
        let a = self.admit(cx, Operation::ListEvents).await?;
        let page = self
            .ledger(&a)
            .list_events(req.limit, req.cursor.as_deref())
            .await
            .map_err(events::read_error)?;
        Ok(EventPage {
            events: page.events.into_iter().map(events::event).collect(),
            next: page.next,
        })
    }

    /// Creates a key in the calling key's environment, holding no scope the caller lacks and
    /// outliving it in no way; the answer holds the key's secret, returned only here.
    ///
    /// # Errors
    ///
    /// As [`Engine::whoami`]; `PermissionDenied` without `admin`; `Invalid` for a request
    /// beyond the caller, an expiry not after now, or a bad label; `ResourceExhausted` while
    /// 1,000 created keys hold a slot: not revoked, and no expiry recorded; `Unauthenticated` if
    /// the caller was revoked or expired before the key was stored. An `Unavailable` of the store
    /// itself says in its hint to list the keys before a retry, and again after a reopen: the key
    /// may be stored (D48).
    ///
    /// # Panics
    ///
    /// If the OS random source fails.
    async fn create_key(&self, cx: &Call, req: CreateKeyRequest) -> Result<CreatedKey, OpError> {
        let a = self.admit(cx, Operation::CreateKey).await?;
        let creator = Emitter::from(a.submit_as());
        let scopes = if req.scopes.is_empty() {
            BTreeSet::from([Scope::Read])
        } else {
            req.scopes
        };
        let (id, key, digest) = open::new_key();
        #[cfg(feature = "__fault-points")]
        ironflowx_core::tell_key(self.env, id, Some(&key));
        let new = NewKey {
            id,
            digest,
            scopes,
            label: req.label,
            expires_at: req.expires_at,
        };
        let (keys, env, clock) = (self.keys(&a), self.env, self.clock.clone());
        let row = key_change(move || keys.insert_key(&env, new, &creator, &*clock, KEY_CAP))
            .await
            .map_err(|e| match e.kind {
                // Its commit may be unsettled (D48), and a retry would make a second key.
                ErrorKind::Unavailable => OpError {
                    hint: Some(
                        "the key may be stored: list the keys before the retry, and again after \
                         the engine reopens; revoke the one no holder has"
                            .into(),
                    ),
                    ..e
                },
                _ => e,
            })?;
        let info = key_info(row, self.clock.now());
        Ok(CreatedKey { key, info })
    }

    /// A page of the calling key's environment's keys, newest first, never a secret.
    ///
    /// # Errors
    ///
    /// As [`Engine::whoami`]; `PermissionDenied` without `admin`; `Invalid` for a cursor that
    /// does not decode or names another incarnation.
    async fn list_keys(&self, cx: &Call, req: ListKeysRequest) -> Result<KeyPage, OpError> {
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
    async fn revoke_key(&self, cx: &Call, req: RevokeKeyRequest) -> Result<(), OpError> {
        let a = self.admit(cx, Operation::RevokeKey).await?;
        let id = req.key_id.parse().map_err(|_| key_not_found())?;
        let by = Emitter::from(a.submit_as());
        let (keys, env, clock) = (self.keys(&a), self.env, self.clock.clone());
        key_change(move || keys.revoke_key(&env, id, &by, &*clock)).await
    }
}

/// What is listed about a key, its status by the clock and its recorded expiry
/// (architecture.md, "Keys").
fn key_info(row: KeyRow, now: Timestamp) -> KeyInfo {
    let r = row.record;
    let status = if r.revoked_at.is_some() {
        KeyStatus::Revoked
    } else if r.expired(now) {
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

/// Runs a control-plane read, or admission's `record_expiry`, off the runtime; any failure is
/// `Unavailable`.
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
