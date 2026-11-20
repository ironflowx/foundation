//! The Ironflow X Operations interface and its request and answer types.
//!
//! Types and a trait only: defaults, limits and every check belong to event rules (D1).
//! No operation takes an environment: an adapter serves exactly one (LED-7).

use std::future::Future;
use std::num::NonZeroU32;

use ironflowx_core::{ErrorKind, EventId, Timestamp};

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
}

/// Who is calling. Opaque and empty in 0.1; 0.2 gives it the credential (D2).
#[derive(Debug, Clone)]
pub struct Call {
    _private: (),
}

impl Call {
    /// A call that carries no credential.
    pub const fn anonymous() -> Self {
        Self { _private: () }
    }
}

/// A failed call: the kind decides whether to retry, the message explains.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpError {
    pub kind: ErrorKind,
    pub message: String,
}

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
