//! The wire: the messages generated from the proto, and their mapping to the domain types. What
//! arrives is validated here, once, for the door and the client alike (D53).

use std::collections::BTreeSet;
use std::fmt;
use std::num::NonZeroU32;

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD};
use buffa::Message as _;
use buffa_types::google::protobuf::Timestamp as WireTime;
use connectrpc::ConnectError;
#[cfg(feature = "server")]
use connectrpc::{ErrorCode, ErrorDetail};
use ironflowx_core::{
    Address, Emitter, ErrorKind, KeyId, KeyOrigin, ParseError, Scope, Secret, Timestamp,
};

use crate::{KeyStatus, OpError, RunStatus};

mod generated {
    include!(concat!(env!("OUT_DIR"), "/wire.rs"));
}

pub use generated::ironflowx::v1::{
    BeginStepRequest, BeginStepResponse, Claim, ClaimRunRequest, ClaimRunResponse,
    CompleteRunRequest, CompleteRunResponse, CompleteStepRequest, CompleteStepResponse,
    CreateKeyRequest, CreateKeyResponse, EmitBatchItem, EmitBatchRequest, EmitBatchResponse,
    EmitRequest, EmitResponse, Ended, Environment, ErrorInfo, Event, Function, GetEventRequest,
    GetFunctionRequest, GetRunRequest, KeyInfo, ListEnvironmentsRequest, ListEnvironmentsResponse,
    ListEventsRequest, ListEventsResponse, ListKeysRequest, ListKeysResponse, PutFunctionRequest,
    PutFunctionResponse, RevokeKeyRequest, RevokeKeyResponse, Run, RunEnd, StartRunRequest,
    StartRunResponse, StepCompleted, StepRun, WhoamiRequest, WhoamiResponse, begin_step_response,
    complete_step_response, emit_batch_item,
};
#[cfg(feature = "server")]
pub use generated::ironflowx::v1::{
    EnvironmentService, EnvironmentServiceServer, EventService, EventServiceServer,
    ExecutionService, ExecutionServiceServer, FunctionService, FunctionServiceServer,
    IdentityService, IdentityServiceServer, KeyService, KeyServiceServer, RunService,
    RunServiceServer,
};
#[cfg(feature = "client")]
pub use generated::ironflowx::v1::{
    EnvironmentServiceClient, EventServiceClient, ExecutionServiceClient, FunctionServiceClient,
    IdentityServiceClient, KeyServiceClient, RunServiceClient,
};

/// The compiled proto, as an encoded `FileDescriptorSet`: every file, its imports and their custom
/// options.
#[cfg(feature = "wire")]
pub const FILE_DESCRIPTOR_SET: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/descriptors.bin"));

/// The most a request body or a message holds, in the door and the client:
/// [`limits::MAX_MESSAGE_BYTES`](crate::limits::MAX_MESSAGE_BYTES).
pub const MAX_MESSAGE: usize = crate::limits::MAX_MESSAGE_BYTES;

/// Sent for a credential that is no key, so the engine answers it as malformed.
pub const MALFORMED_KEY: &str = "malformed";

const ERROR_INFO: &str = "ironflowx.v1.ErrorInfo";

/// An answer whose fields are not what the proto says they hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Undecodable;

impl fmt::Display for Undecodable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("the answer does not decode")
    }
}

impl std::error::Error for Undecodable {}

impl TryFrom<EmitRequest> for crate::EmitRequest {
    type Error = OpError;

    fn try_from(r: EmitRequest) -> Result<Self, OpError> {
        Ok(Self {
            name: r.name,
            data: r.data.map(String::into_bytes),
            metadata: r.metadata.map(String::into_bytes),
            version: r.version,
            idempotency_key: r.idempotency_key,
        })
    }
}

impl TryFrom<crate::EmitRequest> for EmitRequest {
    type Error = OpError;

    fn try_from(r: crate::EmitRequest) -> Result<Self, OpError> {
        let text = |bytes: Option<Vec<u8>>, refused| {
            let text = bytes.map(String::from_utf8).transpose();
            text.map_err(|_| invalid(refused))
        };
        Ok(Self {
            name: r.name,
            data: text(r.data, "event data is not UTF-8")?,
            metadata: text(r.metadata, "event metadata is not UTF-8")?,
            version: r.version,
            idempotency_key: r.idempotency_key,
            ..Self::default()
        })
    }
}

impl From<crate::EmitResponse> for EmitResponse {
    fn from(a: crate::EmitResponse) -> Self {
        Self {
            event_id: a.event_id.to_string(),
            duplicate: a.duplicate,
            ..Self::default()
        }
    }
}

impl TryFrom<EmitResponse> for crate::EmitResponse {
    type Error = Undecodable;

    fn try_from(a: EmitResponse) -> Result<Self, Undecodable> {
        Ok(Self {
            event_id: a.event_id.parse().map_err(|_| Undecodable)?,
            duplicate: a.duplicate,
        })
    }
}

impl TryFrom<EmitBatchRequest> for crate::EmitBatchRequest {
    type Error = OpError;

    fn try_from(r: EmitBatchRequest) -> Result<Self, OpError> {
        let items = r.items.into_iter().map(TryInto::try_into);
        Ok(Self {
            items: items.collect::<Result<_, _>>()?,
        })
    }
}

/// One item the wire cannot carry refuses the whole batch.
impl TryFrom<crate::EmitBatchRequest> for EmitBatchRequest {
    type Error = OpError;

    fn try_from(r: crate::EmitBatchRequest) -> Result<Self, OpError> {
        let items = r.items.into_iter().map(TryInto::try_into);
        Ok(Self {
            items: items.collect::<Result<_, _>>()?,
            ..Self::default()
        })
    }
}

impl From<crate::EmitBatchResponse> for EmitBatchResponse {
    fn from(a: crate::EmitBatchResponse) -> Self {
        let item = |item: Result<crate::EmitResponse, OpError>| EmitBatchItem {
            result: match item {
                Ok(ok) => EmitResponse::from(ok).into(),
                Err(e) => ErrorInfo::from(&e).into(),
            },
            ..EmitBatchItem::default()
        };
        Self {
            items: a.items.into_iter().map(item).collect(),
            ..Self::default()
        }
    }
}

impl TryFrom<EmitBatchResponse> for crate::EmitBatchResponse {
    type Error = Undecodable;

    fn try_from(a: EmitBatchResponse) -> Result<Self, Undecodable> {
        let item = |item: EmitBatchItem| match item.result.ok_or(Undecodable)? {
            emit_batch_item::Result::Ok(ok) => Ok(Ok((*ok).try_into()?)),
            emit_batch_item::Result::Error(e) => Ok(Err((*e).try_into()?)),
        };
        let items = a.items.into_iter().map(item);
        Ok(Self {
            items: items.collect::<Result<_, _>>()?,
        })
    }
}

impl TryFrom<GetEventRequest> for crate::GetEventRequest {
    type Error = OpError;

    fn try_from(r: GetEventRequest) -> Result<Self, OpError> {
        Ok(Self {
            event_id: r.event_id,
        })
    }
}

impl TryFrom<crate::GetEventRequest> for GetEventRequest {
    type Error = OpError;

    fn try_from(r: crate::GetEventRequest) -> Result<Self, OpError> {
        Ok(Self {
            event_id: r.event_id,
            ..Self::default()
        })
    }
}

impl From<crate::Event> for Event {
    fn from(e: crate::Event) -> Self {
        // Not `expect`, which prints the bytes.
        let text = |bytes| match String::from_utf8(bytes) {
            Ok(text) => text,
            Err(_) => panic!("a stored event is not UTF-8 (LED-5)"),
        };
        Self {
            event_id: e.event_id.to_string(),
            name: e.name,
            data: Some(text(e.data)),
            metadata: e.metadata.map(text),
            version: e.version,
            idempotency_key: e.idempotency_key,
            time: Some(sent_time(e.time)).into(),
            emitted_by: e.emitted_by.to_string(),
            ..Self::default()
        }
    }
}

impl TryFrom<Event> for crate::Event {
    type Error = Undecodable;

    fn try_from(e: Event) -> Result<Self, Undecodable> {
        Ok(Self {
            event_id: e.event_id.parse().map_err(|_| Undecodable)?,
            name: e.name,
            data: e.data.ok_or(Undecodable)?.into_bytes(),
            metadata: e.metadata.map(String::into_bytes),
            version: e.version,
            idempotency_key: e.idempotency_key,
            time: read_time(e.time.into_option().ok_or(Undecodable)?)?,
            emitted_by: e.emitted_by.parse().map_err(|_| Undecodable)?,
        })
    }
}

impl TryFrom<ListEventsRequest> for crate::ListEventsRequest {
    type Error = OpError;

    fn try_from(r: ListEventsRequest) -> Result<Self, OpError> {
        Ok(Self {
            limit: read_limit(r.limit)?,
            cursor: r.cursor,
        })
    }
}

impl TryFrom<crate::ListEventsRequest> for ListEventsRequest {
    type Error = OpError;

    fn try_from(r: crate::ListEventsRequest) -> Result<Self, OpError> {
        Ok(Self {
            limit: r.limit.map(NonZeroU32::get),
            cursor: r.cursor,
            ..Self::default()
        })
    }
}

impl From<crate::EventPage> for ListEventsResponse {
    fn from(a: crate::EventPage) -> Self {
        Self {
            events: a.events.into_iter().map(Event::from).collect(),
            next: a.next,
            ..Self::default()
        }
    }
}

impl TryFrom<ListEventsResponse> for crate::EventPage {
    type Error = Undecodable;

    fn try_from(a: ListEventsResponse) -> Result<Self, Undecodable> {
        let events = a.events.into_iter().map(TryInto::try_into);
        Ok(Self {
            events: events.collect::<Result<_, _>>()?,
            next: a.next,
        })
    }
}

impl TryFrom<WhoamiRequest> for crate::WhoamiRequest {
    type Error = OpError;

    fn try_from(_: WhoamiRequest) -> Result<Self, OpError> {
        Ok(Self {})
    }
}

impl TryFrom<crate::WhoamiRequest> for WhoamiRequest {
    type Error = OpError;

    fn try_from(_: crate::WhoamiRequest) -> Result<Self, OpError> {
        Ok(Self::default())
    }
}

impl From<crate::Whoami> for WhoamiResponse {
    fn from(a: crate::Whoami) -> Self {
        Self {
            key_id: a.key_id.map(|id| id.to_string()),
            scopes: sent_scopes(&a.scopes),
            environment: a.environment.to_string(),
            expires_at: a.expires_at.map(sent_time).into(),
            ..Self::default()
        }
    }
}

impl TryFrom<WhoamiResponse> for crate::Whoami {
    type Error = Undecodable;

    fn try_from(a: WhoamiResponse) -> Result<Self, Undecodable> {
        Ok(Self {
            key_id: a
                .key_id
                .map(|id| id.parse())
                .transpose()
                .map_err(|_| Undecodable)?,
            scopes: read_scopes(&a.scopes).map_err(|_| Undecodable)?,
            environment: address(&a.environment)?,
            expires_at: a.expires_at.into_option().map(read_time).transpose()?,
        })
    }
}

impl TryFrom<ListEnvironmentsRequest> for crate::ListEnvironmentsRequest {
    type Error = OpError;

    fn try_from(_: ListEnvironmentsRequest) -> Result<Self, OpError> {
        Ok(Self {})
    }
}

impl TryFrom<crate::ListEnvironmentsRequest> for ListEnvironmentsRequest {
    type Error = OpError;

    fn try_from(_: crate::ListEnvironmentsRequest) -> Result<Self, OpError> {
        Ok(Self::default())
    }
}

impl From<Vec<crate::Environment>> for ListEnvironmentsResponse {
    fn from(a: Vec<crate::Environment>) -> Self {
        Self {
            environments: a.into_iter().map(Environment::from).collect(),
            ..Self::default()
        }
    }
}

impl TryFrom<ListEnvironmentsResponse> for Vec<crate::Environment> {
    type Error = Undecodable;

    fn try_from(a: ListEnvironmentsResponse) -> Result<Self, Undecodable> {
        a.environments.into_iter().map(TryInto::try_into).collect()
    }
}

impl From<crate::Environment> for Environment {
    fn from(e: crate::Environment) -> Self {
        Self {
            env: e.env.to_string(),
            name: e.name,
            address: e.address.to_string(),
            project_id: e.project_id.to_string(),
            created_at: Some(sent_time(e.created_at)).into(),
            ..Self::default()
        }
    }
}

impl TryFrom<Environment> for crate::Environment {
    type Error = Undecodable;

    fn try_from(e: Environment) -> Result<Self, Undecodable> {
        Ok(Self {
            env: e.env.parse().map_err(|_| Undecodable)?,
            name: e.name,
            address: address(&e.address)?,
            project_id: e.project_id.parse().map_err(|_| Undecodable)?,
            created_at: read_time(e.created_at.into_option().ok_or(Undecodable)?)?,
        })
    }
}

impl TryFrom<CreateKeyRequest> for crate::CreateKeyRequest {
    type Error = OpError;

    fn try_from(r: CreateKeyRequest) -> Result<Self, OpError> {
        let expires_at = r.expires_at.into_option().map(read_time).transpose();
        Ok(Self {
            scopes: read_scopes(&r.scopes)
                .map_err(|_| invalid("a scope is read, write, admin or execute"))?,
            expires_at: expires_at.map_err(|_| no_time())?,
            label: r.label,
        })
    }
}

impl TryFrom<crate::CreateKeyRequest> for CreateKeyRequest {
    type Error = OpError;

    fn try_from(r: crate::CreateKeyRequest) -> Result<Self, OpError> {
        // The bound sent for a stored time is no expiry anyone asked for.
        let sent = |at| match read_time(sent_time(at)) {
            Ok(read) if read == at => Ok(sent_time(at)),
            _ => Err(no_time()),
        };
        Ok(Self {
            scopes: sent_scopes(&r.scopes),
            expires_at: r.expires_at.map(sent).transpose()?.into(),
            label: r.label,
            ..Self::default()
        })
    }
}

/// To send, or to print as the answer of `keys create`: the one message that holds a key (TEN-3).
impl From<crate::CreatedKey> for CreateKeyResponse {
    fn from(a: crate::CreatedKey) -> Self {
        Self {
            key: a.key.expose().to_owned(),
            info: Some(KeyInfo::from(a.info)).into(),
            ..Self::default()
        }
    }
}

impl TryFrom<CreateKeyResponse> for crate::CreatedKey {
    type Error = Undecodable;

    fn try_from(a: CreateKeyResponse) -> Result<Self, Undecodable> {
        // First, so that it is zeroed whatever the rest holds.
        let key = Secret::new(a.key);
        let info = a.info.into_option().ok_or(Undecodable)?;
        // `ixk_<key ID>_<secret>` (D46), by the rule credentials parse one: one that no engine
        // accepts, or of another ID, is lost.
        let secret = key
            .expose()
            .strip_prefix("ixk_")
            .and_then(|rest| rest.strip_prefix(&*info.key_id)?.strip_prefix('_'));
        let encoded = |b: u8| b.is_ascii_alphanumeric() || b == b'-' || b == b'_';
        let canonical = |s: &str| s.ends_with(|c| "AEIMQUYcgkosw048".contains(c));
        if !secret.is_some_and(|s| s.len() == 43 && s.bytes().all(encoded) && canonical(s)) {
            return Err(Undecodable);
        }
        Ok(Self {
            key,
            info: info.try_into()?,
        })
    }
}

impl From<crate::KeyInfo> for KeyInfo {
    fn from(k: crate::KeyInfo) -> Self {
        Self {
            key_id: k.key_id.to_string(),
            label: k.label,
            scopes: sent_scopes(&k.scopes),
            origin: origin(k.origin).to_owned(),
            created_by: k.created_by.map(|by| sent_creator(&by)),
            created_at: Some(sent_time(k.created_at)).into(),
            expires_at: k.expires_at.map(sent_time).into(),
            status: status(k.status).to_owned(),
            ..Self::default()
        }
    }
}

impl TryFrom<KeyInfo> for crate::KeyInfo {
    type Error = Undecodable;

    fn try_from(k: KeyInfo) -> Result<Self, Undecodable> {
        Ok(Self {
            key_id: k.key_id.parse().map_err(|_| Undecodable)?,
            label: k.label,
            scopes: read_scopes(&k.scopes).map_err(|_| Undecodable)?,
            origin: ORIGINS
                .into_iter()
                .find(|o| origin(*o) == k.origin)
                .ok_or(Undecodable)?,
            created_by: k.created_by.as_deref().map(read_creator).transpose()?,
            created_at: read_time(k.created_at.into_option().ok_or(Undecodable)?)?,
            expires_at: k.expires_at.into_option().map(read_time).transpose()?,
            status: STATUSES
                .into_iter()
                .find(|s| status(*s) == k.status)
                .ok_or(Undecodable)?,
        })
    }
}

impl TryFrom<ListKeysRequest> for crate::ListKeysRequest {
    type Error = OpError;

    fn try_from(r: ListKeysRequest) -> Result<Self, OpError> {
        Ok(Self {
            limit: read_limit(r.limit)?,
            cursor: r.cursor,
        })
    }
}

impl TryFrom<crate::ListKeysRequest> for ListKeysRequest {
    type Error = OpError;

    fn try_from(r: crate::ListKeysRequest) -> Result<Self, OpError> {
        Ok(Self {
            limit: r.limit.map(NonZeroU32::get),
            cursor: r.cursor,
            ..Self::default()
        })
    }
}

impl From<crate::KeyPage> for ListKeysResponse {
    fn from(a: crate::KeyPage) -> Self {
        Self {
            keys: a.keys.into_iter().map(KeyInfo::from).collect(),
            next: a.next,
            ..Self::default()
        }
    }
}

impl TryFrom<ListKeysResponse> for crate::KeyPage {
    type Error = Undecodable;

    fn try_from(a: ListKeysResponse) -> Result<Self, Undecodable> {
        let keys = a.keys.into_iter().map(TryInto::try_into);
        Ok(Self {
            keys: keys.collect::<Result<_, _>>()?,
            next: a.next,
        })
    }
}

impl TryFrom<RevokeKeyRequest> for crate::RevokeKeyRequest {
    type Error = OpError;

    fn try_from(r: RevokeKeyRequest) -> Result<Self, OpError> {
        Ok(Self { key_id: r.key_id })
    }
}

impl TryFrom<crate::RevokeKeyRequest> for RevokeKeyRequest {
    type Error = OpError;

    fn try_from(r: crate::RevokeKeyRequest) -> Result<Self, OpError> {
        Ok(Self {
            key_id: r.key_id,
            ..Self::default()
        })
    }
}

impl From<()> for RevokeKeyResponse {
    fn from((): ()) -> Self {
        Self::default()
    }
}

impl TryFrom<RevokeKeyResponse> for () {
    type Error = Undecodable;

    fn try_from(_: RevokeKeyResponse) -> Result<Self, Undecodable> {
        Ok(())
    }
}

impl TryFrom<PutFunctionRequest> for crate::PutFunctionRequest {
    type Error = OpError;

    fn try_from(r: PutFunctionRequest) -> Result<Self, OpError> {
        Ok(Self {
            name: r.name,
            code: r.code,
            triggers: r.triggers,
            max_attempts: r.max_attempts,
            first_delay_seconds: r.first_delay_seconds,
            max_delay_seconds: r.max_delay_seconds,
        })
    }
}

impl TryFrom<crate::PutFunctionRequest> for PutFunctionRequest {
    type Error = OpError;

    fn try_from(r: crate::PutFunctionRequest) -> Result<Self, OpError> {
        Ok(Self {
            name: r.name,
            code: r.code,
            triggers: r.triggers,
            max_attempts: r.max_attempts,
            first_delay_seconds: r.first_delay_seconds,
            max_delay_seconds: r.max_delay_seconds,
            ..Self::default()
        })
    }
}

impl From<crate::PutFunctionResponse> for PutFunctionResponse {
    fn from(a: crate::PutFunctionResponse) -> Self {
        Self {
            function: Some(Function::from(a.function)).into(),
            changed: a.changed,
            ..Self::default()
        }
    }
}

impl TryFrom<PutFunctionResponse> for crate::PutFunctionResponse {
    type Error = Undecodable;

    fn try_from(a: PutFunctionResponse) -> Result<Self, Undecodable> {
        Ok(Self {
            function: a.function.into_option().ok_or(Undecodable)?.try_into()?,
            changed: a.changed,
        })
    }
}

impl TryFrom<GetFunctionRequest> for crate::GetFunctionRequest {
    type Error = OpError;

    fn try_from(r: GetFunctionRequest) -> Result<Self, OpError> {
        Ok(Self { name: r.name })
    }
}

impl TryFrom<crate::GetFunctionRequest> for GetFunctionRequest {
    type Error = OpError;

    fn try_from(r: crate::GetFunctionRequest) -> Result<Self, OpError> {
        Ok(Self {
            name: r.name,
            ..Self::default()
        })
    }
}

impl From<crate::Function> for Function {
    fn from(f: crate::Function) -> Self {
        Self {
            name: f.name,
            code: f.code,
            triggers: f.triggers,
            max_attempts: f.max_attempts,
            first_delay_seconds: f.first_delay_seconds,
            max_delay_seconds: f.max_delay_seconds,
            put_at: Some(sent_time(f.put_at)).into(),
            put_by: f.put_by.to_string(),
            ..Self::default()
        }
    }
}

impl TryFrom<Function> for crate::Function {
    type Error = Undecodable;

    fn try_from(f: Function) -> Result<Self, Undecodable> {
        Ok(Self {
            name: f.name,
            code: f.code,
            triggers: f.triggers,
            max_attempts: f.max_attempts,
            first_delay_seconds: f.first_delay_seconds,
            max_delay_seconds: f.max_delay_seconds,
            put_at: read_time(f.put_at.into_option().ok_or(Undecodable)?)?,
            put_by: f.put_by.parse().map_err(|_| Undecodable)?,
        })
    }
}

impl TryFrom<StartRunRequest> for crate::StartRunRequest {
    type Error = OpError;

    fn try_from(r: StartRunRequest) -> Result<Self, OpError> {
        Ok(Self {
            function: r.function,
        })
    }
}

impl TryFrom<crate::StartRunRequest> for StartRunRequest {
    type Error = OpError;

    fn try_from(r: crate::StartRunRequest) -> Result<Self, OpError> {
        Ok(Self {
            function: r.function,
            ..Self::default()
        })
    }
}

impl From<crate::StartRunResponse> for StartRunResponse {
    fn from(a: crate::StartRunResponse) -> Self {
        Self {
            run_id: a.run_id.to_string(),
            duplicate: a.duplicate,
            ..Self::default()
        }
    }
}

impl TryFrom<StartRunResponse> for crate::StartRunResponse {
    type Error = Undecodable;

    fn try_from(a: StartRunResponse) -> Result<Self, Undecodable> {
        Ok(Self {
            run_id: a.run_id.parse().map_err(|_| Undecodable)?,
            duplicate: a.duplicate,
        })
    }
}

impl TryFrom<GetRunRequest> for crate::GetRunRequest {
    type Error = OpError;

    fn try_from(r: GetRunRequest) -> Result<Self, OpError> {
        Ok(Self { run_id: r.run_id })
    }
}

impl TryFrom<crate::GetRunRequest> for GetRunRequest {
    type Error = OpError;

    fn try_from(r: crate::GetRunRequest) -> Result<Self, OpError> {
        Ok(Self {
            run_id: r.run_id,
            ..Self::default()
        })
    }
}

impl From<crate::Run> for Run {
    fn from(r: crate::Run) -> Self {
        Self {
            run_id: r.run_id.to_string(),
            function: r.function,
            code: r.code,
            status: run_status(r.status).to_owned(),
            input: r.input.map(stored_text),
            created_at: Some(sent_time(r.created_at)).into(),
            created_by: r.created_by.to_string(),
            due_at: r.due_at.map(sent_time).into(),
            execution: r.execution,
            lease_until: r.lease_until.map(sent_time).into(),
            steps: r.steps,
            step: r.step,
            output: r.output.map(stored_text),
            ended_at: r.ended_at.map(sent_time).into(),
            ..Self::default()
        }
    }
}

impl TryFrom<Run> for crate::Run {
    type Error = Undecodable;

    fn try_from(r: Run) -> Result<Self, Undecodable> {
        let time = |t: Option<WireTime>| t.map(read_time).transpose();
        Ok(Self {
            run_id: r.run_id.parse().map_err(|_| Undecodable)?,
            function: r.function,
            code: r.code,
            status: read_run_status(&r.status)?,
            input: r.input.map(String::into_bytes),
            created_at: read_time(r.created_at.into_option().ok_or(Undecodable)?)?,
            created_by: r.created_by.parse().map_err(|_| Undecodable)?,
            due_at: time(r.due_at.into_option())?,
            execution: r.execution,
            lease_until: time(r.lease_until.into_option())?,
            steps: r.steps,
            step: r.step,
            output: r.output.map(String::into_bytes),
            ended_at: time(r.ended_at.into_option())?,
        })
    }
}

impl TryFrom<ClaimRunRequest> for crate::ClaimRunRequest {
    type Error = OpError;

    fn try_from(r: ClaimRunRequest) -> Result<Self, OpError> {
        Ok(Self {
            functions: r.functions,
            lease_seconds: r.lease_seconds,
            wait_seconds: r.wait_seconds,
        })
    }
}

impl TryFrom<crate::ClaimRunRequest> for ClaimRunRequest {
    type Error = OpError;

    fn try_from(r: crate::ClaimRunRequest) -> Result<Self, OpError> {
        Ok(Self {
            functions: r.functions,
            lease_seconds: r.lease_seconds,
            wait_seconds: r.wait_seconds,
            ..Self::default()
        })
    }
}

impl From<Option<crate::Claim>> for ClaimRunResponse {
    fn from(a: Option<crate::Claim>) -> Self {
        Self {
            claim: a.map(Claim::from).into(),
            ..Self::default()
        }
    }
}

impl TryFrom<ClaimRunResponse> for Option<crate::Claim> {
    type Error = Undecodable;

    fn try_from(a: ClaimRunResponse) -> Result<Self, Undecodable> {
        a.claim.into_option().map(TryInto::try_into).transpose()
    }
}

impl From<crate::Claim> for Claim {
    fn from(c: crate::Claim) -> Self {
        Self {
            lease: c.lease.to_string(),
            run_id: c.lease.run.to_string(),
            function: c.function,
            code: c.code,
            execution: c.lease.execution,
            input: c.input.map(stored_text),
            lease_until: Some(sent_time(c.lease_until)).into(),
            renew_after_seconds: c.renew_after_seconds,
            ..Self::default()
        }
    }
}

/// The lease, the run ID and the execution number agree.
impl TryFrom<Claim> for crate::Claim {
    type Error = Undecodable;

    fn try_from(c: Claim) -> Result<Self, Undecodable> {
        let lease: ironflowx_core::Lease = c.lease.parse().map_err(|_| Undecodable)?;
        if lease.run.to_string() != c.run_id || lease.execution != c.execution {
            return Err(Undecodable);
        }
        Ok(Self {
            lease,
            function: c.function,
            code: c.code,
            input: c.input.map(String::into_bytes),
            lease_until: read_time(c.lease_until.into_option().ok_or(Undecodable)?)?,
            renew_after_seconds: c.renew_after_seconds,
        })
    }
}

impl TryFrom<BeginStepRequest> for crate::BeginStepRequest {
    type Error = OpError;

    fn try_from(r: BeginStepRequest) -> Result<Self, OpError> {
        Ok(Self {
            lease: r.lease,
            position: r.position,
            name: r.name,
            once: r.once,
            input: r.input.map(String::into_bytes),
        })
    }
}

impl TryFrom<crate::BeginStepRequest> for BeginStepRequest {
    type Error = OpError;

    fn try_from(r: crate::BeginStepRequest) -> Result<Self, OpError> {
        Ok(Self {
            lease: r.lease,
            position: r.position,
            name: r.name,
            once: r.once,
            input: json_text(r.input, "a step input is not UTF-8")?,
            ..Self::default()
        })
    }
}

impl From<crate::BeginStepResponse> for BeginStepResponse {
    fn from(a: crate::BeginStepResponse) -> Self {
        use begin_step_response::Answer;
        let answer = match a {
            crate::BeginStepResponse::Run { attempt } => Answer::from(StepRun {
                attempt,
                ..StepRun::default()
            }),
            crate::BeginStepResponse::Completed { output } => Answer::from(completed(output)),
            crate::BeginStepResponse::Ended(end) => Answer::from(ended(end)),
        };
        Self {
            answer: Some(answer),
            ..Self::default()
        }
    }
}

impl TryFrom<BeginStepResponse> for crate::BeginStepResponse {
    type Error = Undecodable;

    fn try_from(a: BeginStepResponse) -> Result<Self, Undecodable> {
        use begin_step_response::Answer;
        Ok(match a.answer.ok_or(Undecodable)? {
            Answer::Run(run) => Self::Run {
                attempt: run.attempt,
            },
            Answer::Completed(c) => Self::Completed {
                output: read_completed(*c)?,
            },
            Answer::Ended(e) => Self::Ended(read_ended(*e)?),
        })
    }
}

impl TryFrom<CompleteStepRequest> for crate::CompleteStepRequest {
    type Error = OpError;

    fn try_from(r: CompleteStepRequest) -> Result<Self, OpError> {
        Ok(Self {
            lease: r.lease,
            position: r.position,
            output: r.output.map(String::into_bytes),
        })
    }
}

impl TryFrom<crate::CompleteStepRequest> for CompleteStepRequest {
    type Error = OpError;

    fn try_from(r: crate::CompleteStepRequest) -> Result<Self, OpError> {
        Ok(Self {
            lease: r.lease,
            position: r.position,
            output: json_text(r.output, "a step output is not UTF-8")?,
            ..Self::default()
        })
    }
}

impl From<crate::CompleteStepResponse> for CompleteStepResponse {
    fn from(a: crate::CompleteStepResponse) -> Self {
        let crate::CompleteStepResponse::Completed { output } = a;
        Self {
            answer: completed(output).into(),
            ..Self::default()
        }
    }
}

impl TryFrom<CompleteStepResponse> for crate::CompleteStepResponse {
    type Error = Undecodable;

    fn try_from(a: CompleteStepResponse) -> Result<Self, Undecodable> {
        let complete_step_response::Answer::Completed(c) = a.answer.ok_or(Undecodable)?;
        Ok(Self::Completed {
            output: read_completed(*c)?,
        })
    }
}

impl TryFrom<CompleteRunRequest> for crate::CompleteRunRequest {
    type Error = OpError;

    fn try_from(r: CompleteRunRequest) -> Result<Self, OpError> {
        Ok(Self {
            lease: r.lease,
            steps: r.steps,
            output: r.output.map(String::into_bytes),
        })
    }
}

impl TryFrom<crate::CompleteRunRequest> for CompleteRunRequest {
    type Error = OpError;

    fn try_from(r: crate::CompleteRunRequest) -> Result<Self, OpError> {
        Ok(Self {
            lease: r.lease,
            steps: r.steps,
            output: json_text(r.output, "a run output is not UTF-8")?,
            ..Self::default()
        })
    }
}

impl From<crate::RunEnd> for CompleteRunResponse {
    fn from(a: crate::RunEnd) -> Self {
        Self {
            ended: Some(ended(a)).into(),
            ..Self::default()
        }
    }
}

impl TryFrom<CompleteRunResponse> for crate::RunEnd {
    type Error = Undecodable;

    fn try_from(a: CompleteRunResponse) -> Result<Self, Undecodable> {
        read_ended(a.ended.into_option().ok_or(Undecodable)?)
    }
}

fn completed(output: Vec<u8>) -> StepCompleted {
    StepCompleted {
        output: Some(stored_text(output)),
        ..StepCompleted::default()
    }
}

fn read_completed(c: StepCompleted) -> Result<Vec<u8>, Undecodable> {
    c.output.map(String::into_bytes).ok_or(Undecodable)
}

fn ended(end: crate::RunEnd) -> Ended {
    Ended {
        run: Some(RunEnd {
            run_id: end.run_id.to_string(),
            status: run_status(end.status).to_owned(),
            ..RunEnd::default()
        })
        .into(),
        ..Ended::default()
    }
}

fn read_ended(e: Ended) -> Result<crate::RunEnd, Undecodable> {
    let run = e.run.into_option().ok_or(Undecodable)?;
    Ok(crate::RunEnd {
        run_id: run.run_id.parse().map_err(|_| Undecodable)?,
        status: match read_run_status(&run.status)? {
            status @ (RunStatus::Completed | RunStatus::Failed) => status,
            RunStatus::Queued | RunStatus::Running => return Err(Undecodable),
        },
    })
}

/// JSON text to send: bytes that are not UTF-8 are no JSON, and the wire does not carry them.
fn json_text(bytes: Option<Vec<u8>>, refused: &str) -> Result<Option<String>, OpError> {
    let text = bytes.map(String::from_utf8).transpose();
    text.map_err(|_| invalid(refused))
}

/// JSON text the engine stored, which execution rules checked as event data is (LED-5).
fn stored_text(bytes: Vec<u8>) -> String {
    // Not `expect`, which prints the bytes.
    match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(_) => panic!("a stored JSON value is not UTF-8 (LED-5)"),
    }
}

const RUN_STATUSES: [RunStatus; 4] = [
    RunStatus::Queued,
    RunStatus::Running,
    RunStatus::Completed,
    RunStatus::Failed,
];

const fn run_status(status: RunStatus) -> &'static str {
    match status {
        RunStatus::Queued => "queued",
        RunStatus::Running => "running",
        RunStatus::Completed => "completed",
        RunStatus::Failed => "failed",
    }
}

fn read_run_status(text: &str) -> Result<RunStatus, Undecodable> {
    RUN_STATUSES
        .into_iter()
        .find(|s| run_status(*s) == text)
        .ok_or(Undecodable)
}

/// What the wire cannot carry.
fn invalid(message: &str) -> OpError {
    OpError {
        kind: ErrorKind::Invalid,
        message: message.to_owned(),
        hint: None,
    }
}

fn no_time() -> OpError {
    invalid("a time is in whole milliseconds and in years 0001 to 9999")
}

fn read_limit(limit: Option<u32>) -> Result<Option<NonZeroU32>, OpError> {
    let limit = limit.map(|l| NonZeroU32::new(l).ok_or_else(|| invalid("a limit is at least 1")));
    limit.transpose()
}

fn sent_scopes(scopes: &BTreeSet<Scope>) -> Vec<String> {
    scopes.iter().map(ToString::to_string).collect()
}

fn read_scopes(scopes: &[String]) -> Result<BTreeSet<Scope>, ParseError> {
    scopes.iter().map(|s| s.parse()).collect()
}

/// A creator on the wire: a key's bare ID, as since 0.2, or an embedder's `embedder:<name>`
/// (D121).
fn sent_creator(by: &Emitter) -> String {
    by.key_id()
        .map_or_else(|| by.to_string(), |id| id.to_string())
}

fn read_creator(text: &str) -> Result<Emitter, Undecodable> {
    match text.parse::<KeyId>() {
        Ok(id) => Ok(id.into()),
        Err(_) if text.starts_with("embedder:") => text.parse().map_err(|_| Undecodable),
        Err(_) => Err(Undecodable),
    }
}

const ORIGINS: [KeyOrigin; 4] = [
    KeyOrigin::Minted,
    KeyOrigin::Seed,
    KeyOrigin::Recovered,
    KeyOrigin::Created,
];

const fn origin(origin: KeyOrigin) -> &'static str {
    match origin {
        KeyOrigin::Minted => "minted",
        KeyOrigin::Seed => "seed",
        KeyOrigin::Recovered => "recovered",
        KeyOrigin::Created => "created",
    }
}

const STATUSES: [KeyStatus; 3] = [KeyStatus::Active, KeyStatus::Expired, KeyStatus::Revoked];

const fn status(status: KeyStatus) -> &'static str {
    match status {
        KeyStatus::Active => "active",
        KeyStatus::Expired => "expired",
        KeyStatus::Revoked => "revoked",
    }
}

/// An address is split at its first `/`, so it prints back as it was sent (D75).
fn address(text: &str) -> Result<Address, Undecodable> {
    let (project, environment) = text.split_once('/').ok_or(Undecodable)?;
    Ok(Address::new(project, environment))
}

/// 0001-01-01T00:00:00Z and 9999-12-31T23:59:59.999Z, the times the wire carries.
const FIRST_MILLIS: i64 = -62_135_596_800_000;
const LAST_MILLIS: i64 = 253_402_300_799_999;

/// A stored time as the wire carries it: one outside its years as the nearer bound (D76).
fn sent_time(at: Timestamp) -> WireTime {
    let millis = at.as_millis().clamp(FIRST_MILLIS, LAST_MILLIS);
    WireTime {
        seconds: millis.div_euclid(1_000),
        // Below 1e9, so it fits.
        nanos: (millis.rem_euclid(1_000) * 1_000_000) as i32,
        ..WireTime::default()
    }
}

fn read_time(at: WireTime) -> Result<Timestamp, Undecodable> {
    let millis = i64::from(at.nanos) / 1_000_000;
    if !(0..1_000).contains(&millis) || i64::from(at.nanos) != millis * 1_000_000 {
        return Err(Undecodable);
    }
    let millis = at
        .seconds
        .checked_mul(1_000)
        .and_then(|s| s.checked_add(millis))
        .filter(|t| (FIRST_MILLIS..=LAST_MILLIS).contains(t))
        .ok_or(Undecodable)?;
    Ok(Timestamp::from_millis(millis))
}

/// Every kind (architecture.md, Errors).
const KINDS: [ErrorKind; 9] = [
    ErrorKind::Invalid,
    ErrorKind::NotFound,
    ErrorKind::Unknown,
    ErrorKind::Unavailable,
    ErrorKind::Unauthenticated,
    ErrorKind::PermissionDenied,
    ErrorKind::ResourceExhausted,
    ErrorKind::Internal,
    ErrorKind::Conflict,
];

#[cfg(feature = "server")]
const fn connect_code(kind: ErrorKind) -> ErrorCode {
    match kind {
        ErrorKind::Invalid => ErrorCode::InvalidArgument,
        ErrorKind::NotFound => ErrorCode::NotFound,
        ErrorKind::Unknown => ErrorCode::Unknown,
        ErrorKind::Unavailable => ErrorCode::Unavailable,
        ErrorKind::Unauthenticated => ErrorCode::Unauthenticated,
        ErrorKind::PermissionDenied => ErrorCode::PermissionDenied,
        ErrorKind::ResourceExhausted => ErrorCode::ResourceExhausted,
        ErrorKind::Internal => ErrorCode::Internal,
        ErrorKind::Conflict => ErrorCode::FailedPrecondition,
    }
}

impl From<&OpError> for ErrorInfo {
    fn from(e: &OpError) -> Self {
        Self {
            kind: e.kind.as_str().to_owned(),
            message: e.message.clone(),
            hint: e.hint.clone(),
            ..Self::default()
        }
    }
}

impl TryFrom<ErrorInfo> for OpError {
    type Error = Undecodable;

    fn try_from(info: ErrorInfo) -> Result<Self, Undecodable> {
        let of_text = |k: &ErrorKind| k.as_str() == info.kind;
        Ok(Self {
            kind: KINDS.into_iter().find(of_text).ok_or(Undecodable)?,
            message: info.message,
            hint: info.hint,
        })
    }
}

/// The error a door answers: the kind's code, the message, and [`ErrorInfo`] as a detail.
#[cfg(feature = "server")]
pub fn connect_error(e: &OpError) -> ConnectError {
    ConnectError::new(connect_code(e.kind), e.message.clone())
        .with_detail(ErrorDetail::from_message(ERROR_INFO, &ErrorInfo::from(e)))
}

/// The error the engine answered, if the answer carries one: a proxy's, or the library's own,
/// carries none.
pub fn op_error(e: &ConnectError) -> Option<OpError> {
    // gRPC prefixes the name with a host, Connect does not.
    let name = |url: &str| url.rsplit('/').next() == Some(ERROR_INFO);
    let detail = e.details.iter().find(|d| name(&d.type_url))?;
    let value = detail.value.as_ref()?;
    let bytes = STANDARD_NO_PAD
        .decode(value)
        .or_else(|_| STANDARD.decode(value))
        .ok()?;
    ErrorInfo::decode_from_slice(&bytes).ok()?.try_into().ok()
}
