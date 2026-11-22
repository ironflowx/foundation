//! The Ironflow X Operations interface and its request and answer types.
//!
//! Types and a trait only: defaults, limits and every check belong to event rules (D1).
//! No operation takes an environment: an adapter serves exactly one (LED-7).

use std::fmt;
use std::future::Future;
use std::num::NonZeroU32;

use std::collections::BTreeSet;

use ironflowx_core::{
    Address, Emitter, EnvRef, ErrorKind, EventId, KeyId, ProjectId, Scope, Secret, Timestamp,
};

/// The four things a client can do in 0.1. Every caller, in process or remote, goes through
/// this trait. Adapters are generic over it; there is no `dyn` use in 0.1.
pub trait Operations: Send + Sync {
    /// Emits one event.
    fn emit(
        &self,
        cx: &Call,
        req: EmitRequest,
    ) -> impl Future<Output = Result<EmitResponse, OpError>> + Send;

    /// Emits a batch; the answer holds one result per input item, in input order.
    fn emit_batch(
        &self,
        cx: &Call,
        req: EmitBatchRequest,
    ) -> impl Future<Output = Result<EmitBatchResponse, OpError>> + Send;

    /// Reads one event by ID.
    fn get_event(
        &self,
        cx: &Call,
        req: GetEventRequest,
    ) -> impl Future<Output = Result<Event, OpError>> + Send;

    /// Lists a page of events, newest first.
    fn list_events(
        &self,
        cx: &Call,
        req: ListEventsRequest,
    ) -> impl Future<Output = Result<EventPage, OpError>> + Send;

    /// Creates a key in the calling key's environment; the answer holds its secret, once.
    fn create_key(
        &self,
        cx: &Call,
        req: CreateKeyRequest,
    ) -> impl Future<Output = Result<CreatedKey, OpError>> + Send;
}

/// Every operation, for [`required_scope`]. Each operation that lands joins it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Operation {
    Emit,
    EmitBatch,
    GetEvent,
    ListEvents,
    Whoami,
    ListEnvironments,
    CreateKey,
}

/// The scope each operation requires; `None`: any valid key. An exhaustive match, so an
/// operation without a scope does not compile (D37).
pub const fn required_scope(op: Operation) -> Option<Scope> {
    match op {
        Operation::Emit | Operation::EmitBatch => Some(Scope::Write),
        Operation::GetEvent | Operation::ListEvents | Operation::ListEnvironments => {
            Some(Scope::Read)
        }
        Operation::Whoami => None,
        Operation::CreateKey => Some(Scope::Admin),
    }
}

/// Who is calling: the key as presented, or none (D2, D45). Not `Clone`, since the key is a
/// [`Secret`].
pub struct Call {
    key: Option<Secret>,
}

/// Prints no field, so the key cannot reach a log through `{:?}`.
impl fmt::Debug for Call {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Call").finish_non_exhaustive()
    }
}

impl Call {
    /// A call that carries no credential.
    pub const fn anonymous() -> Self {
        Self { key: None }
    }

    /// A call that presents `key`, the whole key text.
    pub const fn with_key(key: Secret) -> Self {
        Self { key: Some(key) }
    }

    /// The key as presented, if any.
    pub const fn key(&self) -> Option<&Secret> {
        self.key.as_ref()
    }
}

/// A failed call: the kind decides whether to retry (`kind.retryable()`), the message explains,
/// and the hint, when there is one, says what to do or when to retry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpError {
    pub kind: ErrorKind,
    pub message: String,
    pub hint: Option<String>,
}

impl fmt::Display for OpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}: {}", self.kind, self.message)
    }
}

impl std::error::Error for OpError {}

/// One event to emit. Absent fields are `None`, never an empty value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmitRequest {
    pub name: String,
    /// The bytes of one JSON value; `None` stores `null` (D22).
    pub data: Option<Vec<u8>>,
    /// The bytes of one JSON object.
    pub metadata: Option<Vec<u8>>,
    /// `None` stores 1.
    pub version: Option<i64>,
    /// `None`: no key.
    pub idempotency_key: Option<String>,
}

/// The answer to an emit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmitResponse {
    pub event_id: EventId,
    /// True when the idempotency key matched an earlier event, whose ID this is.
    pub duplicate: bool,
}

/// Events to emit together.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmitBatchRequest {
    pub items: Vec<EmitRequest>,
}

/// The answer to a batch emit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmitBatchResponse {
    /// One per input item, in input order.
    pub items: Vec<Result<EmitResponse, OpError>>,
}

/// Which event to read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GetEventRequest {
    /// The ID as text; one that does not parse is answered `NotFound`.
    pub event_id: String,
}

/// A stored event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub event_id: EventId,
    pub name: String,
    /// The bytes as sent; the JSON `null` when none were.
    pub data: Vec<u8>,
    pub metadata: Option<Vec<u8>>,
    pub version: i64,
    pub idempotency_key: Option<String>,
    pub time: Timestamp,
    /// Who emitted it; every event of a batch carries the batch's one emitter.
    pub emitted_by: Emitter,
}

/// Which page of events to list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListEventsRequest {
    /// `None` takes event rules' default; zero cannot be expressed.
    pub limit: Option<NonZeroU32>,
    /// The `next` of the previous page; `None` starts at the newest event.
    pub cursor: Option<String>,
}

/// A page of events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventPage {
    /// Newest first.
    pub events: Vec<Event>,
    /// The cursor for the next page; `None` on the last page.
    pub next: Option<String>,
}

/// Asks which key is calling. Takes nothing: a call reaches only its key's environment (LED-7).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WhoamiRequest {}

/// The calling key, as it sees itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Whoami {
    pub key_id: KeyId,
    pub scopes: BTreeSet<Scope>,
    pub environment: Address,
    /// `None`: the key never expires.
    pub expires_at: Option<Timestamp>,
}

/// Asks which environments the calling key reaches: only its own (LED-7).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListEnvironmentsRequest {}

/// An environment a key reaches. It carries its `EnvRef`, never a bare ID (LED-9).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Environment {
    pub env: EnvRef,
    pub name: String,
    pub address: Address,
    pub project_id: ProjectId,
    pub created_at: Timestamp,
}

/// A key to create. No field names an environment: the calling key's is used (LED-7).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CreateKeyRequest {
    /// Empty: `{read}` (D27).
    pub scopes: BTreeSet<Scope>,
    /// `None`: no expiry, allowed only when the creator has none.
    pub expires_at: Option<Timestamp>,
    /// 0–128 bytes of UTF-8, no control character.
    pub label: Option<String>,
}

/// A created key: its secret, returned only here, and what is listed about it. Neither `Clone`
/// nor `PartialEq`, so the secret is not copied or compared:
///
/// ```compile_fail,E0599
/// # use ironflowx_proto::CreatedKey;
/// fn copy(k: CreatedKey) -> (CreatedKey, CreatedKey) { (k.clone(), k) }
/// ```
///
/// ```compile_fail,E0369
/// # fn same(a: &ironflowx_proto::CreatedKey, b: &ironflowx_proto::CreatedKey) -> bool { a == b }
/// ```
pub struct CreatedKey {
    pub key: Secret,
    pub info: KeyInfo,
}

/// Prints the key's information, never the key.
impl fmt::Debug for CreatedKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CreatedKey")
            .field("info", &self.info)
            .finish_non_exhaustive()
    }
}

/// What is known about a key, without its secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyInfo {
    pub key_id: KeyId,
    pub label: Option<String>,
    pub scopes: BTreeSet<Scope>,
    pub origin: KeyOrigin,
    /// The key that created it; `None` for a key no key created.
    pub created_by: Option<KeyId>,
    pub created_at: Timestamp,
    /// `None`: it never expires.
    pub expires_at: Option<Timestamp>,
    pub status: KeyStatus,
}

/// How a key came to be. Minted, seed and recovered keys join when keys are listed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyOrigin {
    /// Created by another key.
    Created,
}

/// Whether a key works. `Expired` and `Revoked` join when keys are listed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyStatus {
    Active,
}
