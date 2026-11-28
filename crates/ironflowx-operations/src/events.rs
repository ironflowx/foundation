//! What the engine's event operations ask of the ledger and answer for it: requests as
//! submissions, outcomes as answers. It holds no rule of its own.

use ironflowx_cell::{ReadError, SubmitError};
use ironflowx_core::ErrorKind;
use ironflowx_events::{Emit, ItemOutcome, Refusal};
use ironflowx_proto::{EmitRequest, EmitResponse, Event, OpError};

/// Runs event rules' checks of one emit without a submit (D120). An emit that admission lets
/// through is refused by event rules, with the same kind and message, exactly when this refuses
/// it, unless event sequences are used up; it reads no key, so it cannot say what admission will
/// answer. A client sizes a payload with it before sending, rather than learning a limit from a
/// refusal. It copies the request to check it.
///
/// # Errors
///
/// The emit's refusal, as [`Operations::emit`](ironflowx_proto::Operations::emit) answers it.
pub fn check_emit(req: &EmitRequest) -> Result<(), OpError> {
    ironflowx_events::check_emit(&emit(req.clone())).map_err(refused)
}

pub(crate) fn emit(req: EmitRequest) -> Emit {
    Emit {
        name: req.name,
        data: req.data,
        metadata: req.metadata,
        version: req.version,
        idempotency_key: req.idempotency_key,
    }
}

/// The engine's one mapping of a rule's refusal to an error, its hint copied.
pub(crate) fn refused(r: Refusal) -> OpError {
    OpError {
        kind: r.kind(),
        message: r.message().to_owned(),
        hint: r.hint().map(str::to_owned),
    }
}

/// What to do about a submission whose outcome is unknown: only an idempotency key keeps a retry
/// from storing a second event.
pub(crate) const fn if_unknown(req: &EmitRequest) -> &'static str {
    match req.idempotency_key {
        Some(_) => "reopen the engine, then retry with the same idempotency key",
        None => "reopen the engine, then check first what landed: a retry may store a second event",
    }
}

/// As [`if_unknown`], of a batch.
pub(crate) const BATCH_IF_UNKNOWN: &str = "reopen the engine, then retry unchanged only if every \
                                           item had an idempotency key and keeps it; else check \
                                           first what landed";

pub(crate) fn submit_error(e: SubmitError, if_unknown: &str) -> OpError {
    match e {
        SubmitError::Refused(r) => refused(r),
        SubmitError::Unknown => OpError {
            kind: ErrorKind::Unknown,
            message: "the outcome is unknown".into(),
            hint: Some(if_unknown.into()),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A refusal's hint reaches the caller's `OpError`, and none is made up.
    #[test]
    fn a_refusals_hint_crosses_into_its_op_error() {
        let base = ironflowx_events::check_emit(&Emit::default()).expect_err("no name");
        let error = |hint: Option<&str>| OpError {
            kind: ErrorKind::Invalid,
            message: "event name is required".into(),
            hint: hint.map(str::to_owned),
        };
        assert_eq!(refused(base.clone()), error(None));
        // A second hint replaces the first.
        let hinted = base.with_hint("first").with_hint("name the event");
        assert_eq!(
            hinted.to_string(),
            "event name is required",
            "Display shows no hint"
        );
        assert_eq!(refused(hinted), error(Some("name the event")));
    }
}
