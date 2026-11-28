//! The Ironflow X Operations interface and its request and answer types.
//!
//! Types, a trait and each operation's required scope (D37), RPC name and whether it changes
//! state. An event's defaults and checks belong to event rules (D1), a key's to the engine,
//! credentials and the control plane; the size limits they check are in [`limits`] (D120).
//! No operation takes an environment: it runs on the one its key is bound to (LED-7).
//!
//! Features, none on by default: `client` is the proto client, [`Operations`] over the network, and
//! `client-tls` adds `https` URLs to it; `wire` is the generated messages of the proto in `proto/`,
//! their mapping to the types here, and the compiled proto; `server` adds the generated service
//! traits a door implements. `wire` and `server` expose types of `buffa` and `connectrpc`, which
//! are 0.x, and follow their versions.

#[cfg(feature = "client")]
mod client;
#[cfg(feature = "wire")]
pub mod wire;
#[cfg(all(feature = "client", not(feature = "wire")))]
mod wire;

use std::any::Any;
use std::fmt;
use std::future::Future;
use std::num::NonZeroU32;
use std::panic::{RefUnwindSafe, UnwindSafe};

use std::collections::BTreeSet;

use ironflowx_core::{
    Address, Emitter, EnvRef, ErrorKind, EventId, KeyId, KeyOrigin, Lease, ProjectId, RunId, Scope,
    Secret, Timestamp,
};

#[cfg(feature = "client")]
pub use client::{Client, InvalidUrl};

/// The size limits of what a request sends, so a client sizes it before sending (D120).
pub use ironflowx_core::limits;

/// Everything a client can do. Every caller, in process or remote, goes through this trait, and
/// a call is answered only for the key it carries (D36). Callers are generic over it; there is
/// no `dyn` use.
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

    /// The calling key, as it sees itself.
    fn whoami(
        &self,
        cx: &Call,
        req: WhoamiRequest,
    ) -> impl Future<Output = Result<Whoami, OpError>> + Send;

    /// Lists the environments the calling key reaches: only its own.
    fn list_environments(
        &self,
        cx: &Call,
        req: ListEnvironmentsRequest,
    ) -> impl Future<Output = Result<Vec<Environment>, OpError>> + Send;

    /// Creates a key in the calling key's environment; the answer holds its secret, once.
    fn create_key(
        &self,
        cx: &Call,
        req: CreateKeyRequest,
    ) -> impl Future<Output = Result<CreatedKey, OpError>> + Send;

    /// Lists a page of the calling key's environment's keys, newest first, never a secret.
    fn list_keys(
        &self,
        cx: &Call,
        req: ListKeysRequest,
    ) -> impl Future<Output = Result<KeyPage, OpError>> + Send;

    /// Revokes a key of the calling key's environment; revoking a revoked key succeeds.
    fn revoke_key(
        &self,
        cx: &Call,
        req: RevokeKeyRequest,
    ) -> impl Future<Output = Result<(), OpError>> + Send;

    /// Puts a function's definition, replacing the whole of one of its name.
    fn put_function(
        &self,
        cx: &Call,
        req: PutFunctionRequest,
    ) -> impl Future<Output = Result<PutFunctionResponse, OpError>> + Send;

    /// Reads one function by name.
    fn get_function(
        &self,
        cx: &Call,
        req: GetFunctionRequest,
    ) -> impl Future<Output = Result<Function, OpError>> + Send;

    /// Starts a run of a function, queued and due at once.
    fn start_run(
        &self,
        cx: &Call,
        req: StartRunRequest,
    ) -> impl Future<Output = Result<StartRunResponse, OpError>> + Send;

    /// Reads one run by ID.
    fn get_run(
        &self,
        cx: &Call,
        req: GetRunRequest,
    ) -> impl Future<Output = Result<Run, OpError>> + Send;

    /// Claims one due run of the functions named, under a lease; `None` when none is due.
    fn claim_run(
        &self,
        cx: &Call,
        req: ClaimRunRequest,
    ) -> impl Future<Output = Result<Option<Claim>, OpError>> + Send;

    /// Asks the engine about a step of a claimed run.
    fn begin_step(
        &self,
        cx: &Call,
        req: BeginStepRequest,
    ) -> impl Future<Output = Result<BeginStepResponse, OpError>> + Send;

    /// Reports a step's output.
    fn complete_step(
        &self,
        cx: &Call,
        req: CompleteStepRequest,
    ) -> impl Future<Output = Result<CompleteStepResponse, OpError>> + Send;

    /// Completes a claimed run with its output.
    fn complete_run(
        &self,
        cx: &Call,
        req: CompleteRunRequest,
    ) -> impl Future<Output = Result<RunEnd, OpError>> + Send;
}

/// Every operation, one per method of [`Operations`]. Each operation that lands joins it, and
/// [`Operation::ALL`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Operation {
    Emit,
    EmitBatch,
    GetEvent,
    ListEvents,
    Whoami,
    ListEnvironments,
    CreateKey,
    ListKeys,
    RevokeKey,
    PutFunction,
    GetFunction,
    StartRun,
    GetRun,
    ClaimRun,
    BeginStep,
    CompleteStep,
    CompleteRun,
}

impl Operation {
    /// Every operation, in the order of the trait's methods.
    pub const ALL: &'static [Self] = &[
        Self::Emit,
        Self::EmitBatch,
        Self::GetEvent,
        Self::ListEvents,
        Self::Whoami,
        Self::ListEnvironments,
        Self::CreateKey,
        Self::ListKeys,
        Self::RevokeKey,
        Self::PutFunction,
        Self::GetFunction,
        Self::StartRun,
        Self::GetRun,
        Self::ClaimRun,
        Self::BeginStep,
        Self::CompleteStep,
        Self::CompleteRun,
    ];

    /// The RPC that carries it on the wire, `<package>.<Service>/<Method>`.
    pub const fn rpc(self) -> &'static str {
        match self {
            Self::Emit => "ironflowx.v1.EventService/Emit",
            Self::EmitBatch => "ironflowx.v1.EventService/EmitBatch",
            Self::GetEvent => "ironflowx.v1.EventService/GetEvent",
            Self::ListEvents => "ironflowx.v1.EventService/ListEvents",
            Self::Whoami => "ironflowx.v1.IdentityService/Whoami",
            Self::ListEnvironments => "ironflowx.v1.EnvironmentService/ListEnvironments",
            Self::CreateKey => "ironflowx.v1.KeyService/CreateKey",
            Self::ListKeys => "ironflowx.v1.KeyService/ListKeys",
            Self::RevokeKey => "ironflowx.v1.KeyService/RevokeKey",
            Self::PutFunction => "ironflowx.v1.FunctionService/PutFunction",
            Self::GetFunction => "ironflowx.v1.FunctionService/GetFunction",
            Self::StartRun => "ironflowx.v1.RunService/StartRun",
            Self::GetRun => "ironflowx.v1.RunService/GetRun",
            Self::ClaimRun => "ironflowx.v1.ExecutionService/ClaimRun",
            Self::BeginStep => "ironflowx.v1.ExecutionService/BeginStep",
            Self::CompleteStep => "ironflowx.v1.ExecutionService/CompleteStep",
            Self::CompleteRun => "ironflowx.v1.ExecutionService/CompleteRun",
        }
    }

    /// True when the operation may append to the ledger or change keys: every one but the reads.
    /// Any call may still record its own key's expiry as it is refused (D42).
    pub const fn changes_state(self) -> bool {
        match self {
            Self::Emit
            | Self::EmitBatch
            | Self::CreateKey
            | Self::RevokeKey
            | Self::PutFunction
            | Self::StartRun
            | Self::ClaimRun
            | Self::BeginStep
            | Self::CompleteStep
            | Self::CompleteRun => true,
            Self::GetEvent
            | Self::ListEvents
            | Self::Whoami
            | Self::ListEnvironments
            | Self::ListKeys
            | Self::GetFunction
            | Self::GetRun => false,
        }
    }
}

/// The scope each operation requires; `None`: any valid key. An exhaustive match, so an
/// operation without a scope does not compile (D37).
pub const fn required_scope(op: Operation) -> Option<Scope> {
    match op {
        Operation::Emit | Operation::EmitBatch | Operation::PutFunction | Operation::StartRun => {
            Some(Scope::Write)
        }
        Operation::GetEvent
        | Operation::ListEvents
        | Operation::ListEnvironments
        | Operation::GetFunction
        | Operation::GetRun => Some(Scope::Read),
        Operation::Whoami => None,
        Operation::CreateKey | Operation::ListKeys | Operation::RevokeKey => Some(Scope::Admin),
        Operation::ClaimRun
        | Operation::BeginStep
        | Operation::CompleteStep
        | Operation::CompleteRun => Some(Scope::Execute),
    }
}

/// Who is calling: the key as presented, a grant in its place, or neither (D2, D45, D121). Not
/// `Clone`, since the key is a [`Secret`]:
///
/// ```compile_fail,E0599
/// # use ironflowx_proto::Call;
/// fn copy(c: Call) -> (Call, Call) { (c.clone(), c) }
/// ```
pub struct Call {
    key: Option<Secret>,
    /// Unwind-safe as a key is, so `Call` keeps the auto traits it had before it held one.
    grant: Option<Box<dyn Any + Send + Sync + UnwindSafe + RefUnwindSafe>>,
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
        Self {
            key: None,
            grant: None,
        }
    }

    /// A call that presents `key`, the whole key text.
    pub const fn with_key(key: Secret) -> Self {
        Self {
            key: Some(key),
            grant: None,
        }
    }

    /// A call that carries `grant` in place of a key: a value of a type only its maker can name,
    /// as the engine's `embedder_call` makes, so only that maker reads it. No wire message has a
    /// field for it, so no door makes one (D121).
    pub fn with_grant(grant: impl Any + Send + Sync + UnwindSafe + RefUnwindSafe) -> Self {
        Self {
            key: None,
            grant: Some(Box::new(grant)),
        }
    }

    /// The key as presented, if any.
    pub const fn key(&self) -> Option<&Secret> {
        self.key.as_ref()
    }

    /// The grant it carries, if any.
    pub fn grant(&self) -> Option<&(dyn Any + Send + Sync)> {
        self.grant.as_deref().map(|g| g as &(dyn Any + Send + Sync))
    }
}

/// A failed call: `kind.retryable()` says whether any call that failed so may be sent again
/// unchanged, the message explains, and the hint, when there is one, says what to do.
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

/// A page of events. It can hold fewer than the limit and not be the last: it stops before its
/// data and metadata pass 3 MiB, though a page holds at least one event if any is left.
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
    /// `None` for an embedder's call, which holds no key and which no door carries (D121).
    pub key_id: Option<KeyId>,
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
    /// 0 to [`limits::MAX_KEY_LABEL_BYTES`] bytes of UTF-8, no control character.
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
    /// The key, or the embedder (D121), that created it; `None` for a key neither created.
    pub created_by: Option<Emitter>,
    pub created_at: Timestamp,
    /// `None`: it never expires.
    pub expires_at: Option<Timestamp>,
    pub status: KeyStatus,
}

/// Whether a key works: `Revoked` once revoked, whatever its expiry; otherwise `Expired` from
/// its expiry, or once refused as expired; otherwise `Active`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyStatus {
    Active,
    Expired,
    Revoked,
}

/// Which page of keys to list.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListKeysRequest {
    /// `None` takes the engine's default, 100; above 1,000 clamps to 1,000.
    pub limit: Option<NonZeroU32>,
    /// The `next` of the previous page; `None` starts at the newest key.
    pub cursor: Option<String>,
}

/// A page of keys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyPage {
    /// Newest first.
    pub keys: Vec<KeyInfo>,
    /// The cursor for the next page; `None` on the last page.
    pub next: Option<String>,
}

/// Which key to revoke.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevokeKeyRequest {
    /// The key ID as text; one that does not parse is answered `NotFound`.
    pub key_id: String,
}

/// A function's definition to put. It replaces the whole definition of its name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PutFunctionRequest {
    /// 1 to [`limits::MAX_FUNCTION_NAME_BYTES`] bytes of `[A-Za-z0-9._-]`.
    pub name: String,
    /// The code label, likewise.
    pub code: String,
    /// At most 10 exact event names the engine does not own, none repeated.
    pub triggers: Vec<String>,
    pub max_attempts: Option<u32>,
    pub first_delay_seconds: Option<u32>,
    pub max_delay_seconds: Option<u32>,
}

/// The answer to a put.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PutFunctionResponse {
    pub function: Function,
    /// False when the definition was already the one put: nothing was appended.
    pub changed: bool,
}

/// Which function to read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GetFunctionRequest {
    pub name: String,
}

/// A function as put. A policy field is `None` when it was not put.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Function {
    pub name: String,
    pub code: String,
    /// In name order.
    pub triggers: Vec<String>,
    pub max_attempts: Option<u32>,
    pub first_delay_seconds: Option<u32>,
    pub max_delay_seconds: Option<u32>,
    pub put_at: Timestamp,
    pub put_by: Emitter,
}

/// A run to start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartRunRequest {
    /// The function's name; one that does not exist is answered `NotFound`.
    pub function: String,
}

/// The answer to a start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartRunResponse {
    pub run_id: RunId,
    /// Always false until starts take an idempotency key.
    pub duplicate: bool,
}

/// Which run to read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GetRunRequest {
    /// The ID as text; one that does not parse is answered `NotFound`.
    pub run_id: String,
}

/// A run's status. It changes only when a record commits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RunStatus {
    Queued,
    Running,
    Completed,
    Failed,
}

/// A run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub run_id: RunId,
    pub function: String,
    /// The function's code label when the run was created.
    pub code: String,
    pub status: RunStatus,
    /// JSON text.
    pub input: Option<Vec<u8>>,
    pub created_at: Timestamp,
    pub created_by: Emitter,
    /// While queued.
    pub due_at: Option<Timestamp>,
    /// The current execution's number; 0 before the first claim.
    pub execution: u32,
    /// While running.
    pub lease_until: Option<Timestamp>,
    /// How many steps have been begun.
    pub steps: u32,
    /// The open step's name, if one is open.
    pub step: Option<String>,
    /// JSON text, once completed.
    pub output: Option<Vec<u8>>,
    pub ended_at: Option<Timestamp>,
}

/// A claim: the functions it takes a run of, as `name@code`, and the lease it asks for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClaimRunRequest {
    /// 1 to 100 different `name@code` pairs.
    pub functions: Vec<String>,
    /// 10 to 900; `None`: 60.
    pub lease_seconds: Option<u32>,
    /// 0 to 20. The claim answers at once whatever it says (the wait arrives later).
    pub wait_seconds: Option<u32>,
}

/// A run a claim took.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claim {
    /// The run, and this execution's number.
    pub lease: Lease,
    pub function: String,
    pub code: String,
    /// JSON text.
    pub input: Option<Vec<u8>>,
    pub lease_until: Timestamp,
    /// A third of the lease's length.
    pub renew_after_seconds: u32,
}

/// A question about a step of a claimed run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BeginStepRequest {
    /// `<run-id>.<execution>`, as a claim answered it.
    pub lease: String,
    /// From 1: the executor's count of its step calls.
    pub position: u32,
    pub name: String,
    pub once: bool,
    /// JSON text.
    pub input: Option<Vec<u8>>,
}

/// What a step call answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BeginStepResponse {
    /// Do the work.
    Run { attempt: u32 },
    /// The step's recorded output, byte for byte: do not do the work.
    Completed { output: Vec<u8> },
    /// The run has ended: stop.
    Ended(RunEnd),
}

/// A run that has ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunEnd {
    pub run_id: RunId,
    pub status: RunStatus,
}

/// A step's output.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompleteStepRequest {
    pub lease: String,
    pub position: u32,
    /// JSON text; `None` stores `null`.
    pub output: Option<Vec<u8>>,
}

/// What reporting a step's output answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompleteStepResponse {
    /// The step's recorded output: this one, or the first one reported.
    Completed { output: Vec<u8> },
}

/// A run's completion.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompleteRunRequest {
    pub lease: String,
    /// How many step positions this execution reached.
    pub steps: u32,
    /// JSON text; `None` stores `null`.
    pub output: Option<Vec<u8>>,
}
