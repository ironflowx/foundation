//! `LocalOperations`: the `Operations` interface over one ledger. It maps requests to
//! submissions and queries and outcomes to answers, and holds no rule of its own.

use ironflowx_core::{ErrorKind, SubmitAs};
use ironflowx_events::{Emit, ItemOutcome, Refusal, Submission};
use ironflowx_proto::{
    Call, EmitBatchRequest, EmitBatchResponse, EmitRequest, EmitResponse, Event, EventPage,
    GetEventRequest, ListEventsRequest, OpError, Operations,
};

use crate::{Ledger, ReadError, SubmitError};

/// An in-process client of one ledger. Several can share it.
#[derive(Clone, Copy)]
pub struct LocalOperations<'a> {
    ledger: &'a Ledger,
}

impl<'a> LocalOperations<'a> {
    pub const fn new(ledger: &'a Ledger) -> Self {
        Self { ledger }
    }
}

/// Until the engine replaces it, every submission is attributed to this embedder (D36).
fn local() -> SubmitAs {
    SubmitAs::embedder("local-operations").expect("a valid embedder name")
}

fn emit(req: EmitRequest) -> Emit {
    Emit {
        name: req.name,
        data: req.data,
        metadata: req.metadata,
        version: req.version,
        idempotency_key: req.idempotency_key,
    }
}

fn refused(r: Refusal) -> OpError {
    OpError {
        kind: r.kind(),
        message: r.message().to_owned(),
        hint: None,
    }
}

fn submit_error(e: SubmitError) -> OpError {
    match e {
        SubmitError::Refused(r) => refused(r),
        SubmitError::Unknown => OpError {
            kind: ErrorKind::Unknown,
            message: "the outcome is unknown; retry with the idempotency key after reopen".into(),
            hint: Some("reopen the ledger, then retry with the same idempotency key".into()),
        },
        SubmitError::Unavailable => unavailable(),
    }
}

fn read_error(e: ReadError) -> OpError {
    match e {
        ReadError::Refused(r) => refused(r),
        ReadError::Unavailable => unavailable(),
    }
}

fn unavailable() -> OpError {
    OpError {
        kind: ErrorKind::Unavailable,
        message: "the ledger is unavailable until it is reopened".into(),
        hint: Some("reopen the ledger, then retry".into()),
    }
}

fn response(outcome: ItemOutcome) -> Result<EmitResponse, OpError> {
    match outcome {
        ItemOutcome::New(event_id) => Ok(EmitResponse {
            event_id,
            duplicate: false,
        }),
        ItemOutcome::Duplicate(event_id) => Ok(EmitResponse {
            event_id,
            duplicate: true,
        }),
        ItemOutcome::Refused(r) => Err(refused(r)),
    }
}

fn event(e: ironflowx_events::Event) -> Event {
    Event {
        event_id: e.id,
        name: e.name,
        data: e.data,
        metadata: e.metadata,
        version: e.version,
        idempotency_key: e.idempotency_key,
        time: e.time,
        emitted_by: e.emitted_by,
    }
}

impl Operations for LocalOperations<'_> {
    async fn emit(&self, _cx: &Call, req: EmitRequest) -> Result<EmitResponse, OpError> {
        let committed = self
            .ledger
            .submit(Submission::Emit(emit(req)), local())
            .await
            .map_err(submit_error)?;
        let [outcome] = <[_; 1]>::try_from(committed.outcome).expect("one outcome per emit");
        response(outcome)
    }

    async fn emit_batch(
        &self,
        _cx: &Call,
        req: EmitBatchRequest,
    ) -> Result<EmitBatchResponse, OpError> {
        let items = req.items.into_iter().map(emit).collect();
        let committed = self
            .ledger
            .submit(Submission::Batch(items), local())
            .await
            .map_err(submit_error)?;
        Ok(EmitBatchResponse {
            items: committed.outcome.into_iter().map(response).collect(),
        })
    }

    async fn get_event(&self, _cx: &Call, req: GetEventRequest) -> Result<Event, OpError> {
        self.ledger
            .read(move |v| ironflowx_events::get_event(v, &req.event_id))
            .await
            .map(event)
            .map_err(read_error)
    }

    async fn list_events(&self, _cx: &Call, req: ListEventsRequest) -> Result<EventPage, OpError> {
        let token = self.ledger.env().token();
        let page = self
            .ledger
            .read(move |v| {
                ironflowx_events::list_events(v, token, req.limit, req.cursor.as_deref())
            })
            .await
            .map_err(read_error)?;
        Ok(EventPage {
            events: page.events.into_iter().map(event).collect(),
            next: page.next,
        })
    }
}
