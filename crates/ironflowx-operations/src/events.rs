//! What the engine's event operations ask of the ledger and answer for it: requests as
//! submissions, outcomes as answers. It holds no rule of its own.

use ironflowx_cell::{ReadError, SubmitError};
use ironflowx_core::ErrorKind;
use ironflowx_events::{Emit, ItemOutcome, Refusal};
use ironflowx_proto::{EmitRequest, EmitResponse, Event, OpError};

pub(crate) fn emit(req: EmitRequest) -> Emit {
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

pub(crate) fn submit_error(e: SubmitError) -> OpError {
    match e {
        SubmitError::Refused(r) => refused(r),
        SubmitError::Unknown => OpError {
            kind: ErrorKind::Unknown,
            message: "the outcome is unknown; retry with the idempotency key after reopen".into(),
            hint: Some("reopen the engine, then retry with the same idempotency key".into()),
        },
        SubmitError::Unavailable => unavailable(),
    }
}

pub(crate) fn read_error(e: ReadError) -> OpError {
    match e {
        ReadError::Refused(r) => refused(r),
        ReadError::Unavailable => unavailable(),
    }
}

fn unavailable() -> OpError {
    OpError {
        kind: ErrorKind::Unavailable,
        message: "the ledger is unavailable until it is reopened".into(),
        hint: Some("reopen the engine, then retry".into()),
    }
}

pub(crate) fn response(outcome: ItemOutcome) -> Result<EmitResponse, OpError> {
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

pub(crate) fn event(e: ironflowx_events::Event) -> Event {
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
