//! The HTTP door: Connect, gRPC and gRPC-Web on the same handlers, with `/health` and `/ready`.

use std::sync::{Arc, OnceLock};
use std::time::Instant;

use axum::Router;
use axum::extract::Request;
use axum::middleware::{self, Next};
use axum::response::Response as HttpResponse;
use axum::routing::get;
use connectrpc::{
    Chain, CompressionRegistry, ConnectError, ConnectRpcService, Encodable, Limits, RequestContext,
    Response, ServiceRequest, ServiceResult,
};
use http::header::{AUTHORIZATION, CONTENT_TYPE};
use http::{HeaderMap, HeaderName, StatusCode};
use ironflowx_core::{ErrorKind, Secret};
use ironflowx_proto::wire::{
    self, BeginStepRequest, BeginStepResponse, ClaimRunRequest, ClaimRunResponse,
    CompleteRunRequest, CompleteRunResponse, CompleteStepRequest, CompleteStepResponse,
    CreateKeyRequest, CreateKeyResponse, EmitBatchRequest, EmitBatchResponse, EmitRequest,
    EmitResponse, EnvironmentService, EnvironmentServiceServer, Event, EventService,
    EventServiceServer, ExecutionService, ExecutionServiceServer, Function, FunctionService,
    FunctionServiceServer, GetEventRequest, GetFunctionRequest, GetRunRequest, IdentityService,
    IdentityServiceServer, KeyService, KeyServiceServer, ListEnvironmentsRequest,
    ListEnvironmentsResponse, ListEventsRequest, ListEventsResponse, ListFunctionsRequest,
    ListFunctionsResponse, ListKeysRequest, ListKeysResponse, PutFunctionRequest,
    PutFunctionResponse, RevokeKeyRequest, RevokeKeyResponse, Run, RunService, RunServiceServer,
    StartRunRequest, StartRunResponse, WhoamiRequest, WhoamiResponse,
};
use ironflowx_proto::{Call, OpError, Operation, Operations};

use crate::{Readiness, Slot, State, WaitingOn};

/// The HTTP door of the engine in `slot`: its RPCs, `/health` and `/ready`.
pub fn router<O: Operations + 'static, R: Readiness + 'static>(slot: Arc<Slot<O, R>>) -> Router {
    let limits = Limits::default()
        .with_max_request_body_size(wire::MAX_MESSAGE)
        .with_max_message_size(wire::MAX_MESSAGE);
    let events = EventServiceServer::new(Door(slot.clone()));
    let identity = IdentityServiceServer::new(Door(slot.clone()));
    let environments = EnvironmentServiceServer::new(Door(slot.clone()));
    let keys = KeyServiceServer::new(Door(slot.clone()));
    let functions = FunctionServiceServer::new(Door(slot.clone()));
    let runs = RunServiceServer::new(Door(slot.clone()));
    let executions = ExecutionServiceServer::new(Door(slot.clone()));
    let services = Chain(
        events,
        Chain(
            identity,
            Chain(
                environments,
                Chain(keys, Chain(functions, Chain(runs, executions))),
            ),
        ),
    );
    let rpcs = ConnectRpcService::new(services)
        .with_limits(limits)
        // Another crate of the build may switch gzip or zstd on in `connectrpc`.
        .with_compression(CompressionRegistry::new());
    Router::new()
        .route("/health", get(|| async { StatusCode::OK }))
        .route("/ready", get(move || async move { ready(&slot).await }))
        .fallback_service(rpcs)
        .layer(middleware::from_fn(log_refusal))
}

/// Shared with the generated handler through the request extensions. An operation owns its
/// own log once it is admitted. A wire refusal that doesn't reach the handler has no domain kind.
#[derive(Clone, Default)]
struct Dispatch(Arc<OnceLock<DispatchOutcome>>);

enum DispatchOutcome {
    Admitted,
    Refused(ErrorKind),
}

/// Logs once unless admission handed the record to the Operations adapter.
struct RefusalLog {
    dispatch: Dispatch,
    operation: &'static str,
    started: Instant,
    finished: bool,
}

impl Drop for RefusalLog {
    fn drop(&mut self) {
        let outcome = match self.dispatch.0.get() {
            Some(DispatchOutcome::Admitted) => return,
            Some(DispatchOutcome::Refused(kind)) => kind.as_str(),
            None if self.finished => "refused",
            None => "cancelled",
        };
        tracing::info!(
            operation = self.operation,
            outcome,
            duration_ms = self.started.elapsed().as_secs_f64() * 1000.0,
            "call"
        );
    }
}

async fn log_refusal(mut req: Request, next: Next) -> HttpResponse {
    let path = req.uri().path().strip_prefix('/').unwrap_or_default();
    if !path.starts_with("ironflowx.v1.") {
        return next.run(req).await;
    }
    let operation = Operation::ALL
        .iter()
        .find(|op| op.rpc() == path)
        .map_or("unknown", |op| op.rpc());
    let dispatch = Dispatch::default();
    req.extensions_mut().insert(dispatch.clone());
    let mut log = RefusalLog {
        dispatch,
        operation,
        started: Instant::now(),
        finished: false,
    };
    let response = next.run(req).await;
    log.finished = true;
    response
}

type Json = (StatusCode, [(HeaderName, &'static str); 1], &'static str);

async fn ready<O: Operations, R: Readiness>(slot: &Slot<O, R>) -> Json {
    let (status, body) = match slot.ready().await {
        Ok(()) => (StatusCode::OK, r#"{"ready":true}"#),
        Err(waiting_on) => (
            StatusCode::SERVICE_UNAVAILABLE,
            match waiting_on {
                WaitingOn::Open => r#"{"ready":false,"waiting_on":"open"}"#,
                WaitingOn::ControlPlane => r#"{"ready":false,"waiting_on":"control_plane"}"#,
                WaitingOn::Ledger => r#"{"ready":false,"waiting_on":"ledger"}"#,
            },
        ),
    };
    (status, [(CONTENT_TYPE, "application/json")], body)
}

impl<O: Operations, R: Readiness> Slot<O, R> {
    fn operations(&self) -> Result<Arc<O>, OpError> {
        let state = self.0.lock().expect("slot lock");
        let hint = match &*state {
            State::Open(operations, _) => return Ok(operations.clone()),
            State::Starting => "the engine is starting",
            State::Draining | State::Closed => "the engine is stopping",
        };
        Err(OpError {
            kind: ErrorKind::Unavailable,
            message: "the engine is unavailable".into(),
            hint: Some(hint.into()),
        })
    }

    async fn ready(&self) -> Result<(), WaitingOn> {
        let readiness = match &*self.0.lock().expect("slot lock") {
            State::Open(_, readiness) => readiness.clone(),
            _ => return Err(WaitingOn::Open),
        };
        let health = readiness.ready().await;
        // The health read may have waited while the host stopped admitting calls.
        if matches!(*self.0.lock().expect("slot lock"), State::Open(..)) {
            health
        } else {
            Err(WaitingOn::Open)
        }
    }
}

fn refused(e: OpError) -> ConnectError {
    wire::connect_error(&e)
}

struct Door<O: Operations, R: Readiness>(Arc<Slot<O, R>>);

impl<O: Operations, R: Readiness> Door<O, R> {
    /// Finishes validation before letting the operation take over the call.
    fn admit<T, U>(&self, ctx: &RequestContext, request: T) -> Result<(Arc<O>, U), ConnectError>
    where
        T: TryInto<U, Error = OpError>,
    {
        let checked = request
            .try_into()
            .and_then(|request| self.0.operations().map(|operations| (operations, request)));
        if let Some(dispatch) = ctx.extensions().get::<Dispatch>() {
            let outcome = match &checked {
                Ok(_) => DispatchOutcome::Admitted,
                Err(e) => DispatchOutcome::Refused(e.kind),
            };
            let _ = dispatch.0.set(outcome);
        }
        checked.map_err(refused)
    }
}

impl<O: Operations + 'static, R: Readiness + 'static> EventService for Door<O, R> {
    async fn emit<'a>(
        &'a self,
        ctx: RequestContext,
        request: ServiceRequest<'_, EmitRequest>,
    ) -> ServiceResult<impl Encodable<EmitResponse> + Send + use<'a, O, R>> {
        let (operations, request) = self.admit(&ctx, request.to_owned_message())?;
        let answer = operations.emit(&call(ctx.headers()), request).await;
        Response::ok(EmitResponse::from(answer.map_err(refused)?))
    }

    async fn emit_batch<'a>(
        &'a self,
        ctx: RequestContext,
        request: ServiceRequest<'_, EmitBatchRequest>,
    ) -> ServiceResult<impl Encodable<EmitBatchResponse> + Send + use<'a, O, R>> {
        let (operations, request) = self.admit(&ctx, request.to_owned_message())?;
        let answer = operations.emit_batch(&call(ctx.headers()), request).await;
        Response::ok(EmitBatchResponse::from(answer.map_err(refused)?))
    }

    async fn get_event<'a>(
        &'a self,
        ctx: RequestContext,
        request: ServiceRequest<'_, GetEventRequest>,
    ) -> ServiceResult<impl Encodable<Event> + Send + use<'a, O, R>> {
        let (operations, request) = self.admit(&ctx, request.to_owned_message())?;
        let answer = operations.get_event(&call(ctx.headers()), request).await;
        Response::ok(Event::from(answer.map_err(refused)?))
    }

    async fn list_events<'a>(
        &'a self,
        ctx: RequestContext,
        request: ServiceRequest<'_, ListEventsRequest>,
    ) -> ServiceResult<impl Encodable<ListEventsResponse> + Send + use<'a, O, R>> {
        let (operations, request) = self.admit(&ctx, request.to_owned_message())?;
        let answer = operations.list_events(&call(ctx.headers()), request).await;
        Response::ok(ListEventsResponse::from(answer.map_err(refused)?))
    }
}

impl<O: Operations + 'static, R: Readiness + 'static> IdentityService for Door<O, R> {
    async fn whoami<'a>(
        &'a self,
        ctx: RequestContext,
        request: ServiceRequest<'_, WhoamiRequest>,
    ) -> ServiceResult<impl Encodable<WhoamiResponse> + Send + use<'a, O, R>> {
        let (operations, request) = self.admit(&ctx, request.to_owned_message())?;
        let answer = operations.whoami(&call(ctx.headers()), request).await;
        Response::ok(WhoamiResponse::from(answer.map_err(refused)?))
    }
}

impl<O: Operations + 'static, R: Readiness + 'static> EnvironmentService for Door<O, R> {
    async fn list_environments<'a>(
        &'a self,
        ctx: RequestContext,
        request: ServiceRequest<'_, ListEnvironmentsRequest>,
    ) -> ServiceResult<impl Encodable<ListEnvironmentsResponse> + Send + use<'a, O, R>> {
        let (operations, request) = self.admit(&ctx, request.to_owned_message())?;
        let answer = operations
            .list_environments(&call(ctx.headers()), request)
            .await;
        Response::ok(ListEnvironmentsResponse::from(answer.map_err(refused)?))
    }
}

impl<O: Operations + 'static, R: Readiness + 'static> KeyService for Door<O, R> {
    async fn create_key<'a>(
        &'a self,
        ctx: RequestContext,
        request: ServiceRequest<'_, CreateKeyRequest>,
    ) -> ServiceResult<impl Encodable<CreateKeyResponse> + Send + use<'a, O, R>> {
        let (operations, request) = self.admit(&ctx, request.to_owned_message())?;
        let answer = operations.create_key(&call(ctx.headers()), request).await;
        Response::ok(CreateKeyResponse::from(answer.map_err(refused)?))
    }

    async fn list_keys<'a>(
        &'a self,
        ctx: RequestContext,
        request: ServiceRequest<'_, ListKeysRequest>,
    ) -> ServiceResult<impl Encodable<ListKeysResponse> + Send + use<'a, O, R>> {
        let (operations, request) = self.admit(&ctx, request.to_owned_message())?;
        let answer = operations.list_keys(&call(ctx.headers()), request).await;
        Response::ok(ListKeysResponse::from(answer.map_err(refused)?))
    }

    async fn revoke_key<'a>(
        &'a self,
        ctx: RequestContext,
        request: ServiceRequest<'_, RevokeKeyRequest>,
    ) -> ServiceResult<impl Encodable<RevokeKeyResponse> + Send + use<'a, O, R>> {
        let (operations, request) = self.admit(&ctx, request.to_owned_message())?;
        let answer = operations.revoke_key(&call(ctx.headers()), request).await;
        Response::ok(RevokeKeyResponse::from(answer.map_err(refused)?))
    }
}

/// A handler: admits the request, calls the operation once, answers what it answered.
macro_rules! handlers {
    ($service:ident { $($name:ident($request:ty) -> $answer:ty;)* }) => {
        impl<O: Operations + 'static, R: Readiness + 'static> $service for Door<O, R> {
            $(async fn $name<'a>(
                &'a self,
                ctx: RequestContext,
                request: ServiceRequest<'_, $request>,
            ) -> ServiceResult<impl Encodable<$answer> + Send + use<'a, O, R>> {
                let (operations, request) = self.admit(&ctx, request.to_owned_message())?;
                let answer = operations.$name(&call(ctx.headers()), request).await;
                Response::ok(<$answer>::from(answer.map_err(refused)?))
            })*
        }
    };
}

handlers!(FunctionService {
    put_function(PutFunctionRequest) -> PutFunctionResponse;
    get_function(GetFunctionRequest) -> Function;
    list_functions(ListFunctionsRequest) -> ListFunctionsResponse;
});

handlers!(RunService {
    start_run(StartRunRequest) -> StartRunResponse;
    get_run(GetRunRequest) -> Run;
});

handlers!(ExecutionService {
    claim_run(ClaimRunRequest) -> ClaimRunResponse;
    begin_step(BeginStepRequest) -> BeginStepResponse;
    complete_step(CompleteStepRequest) -> CompleteStepResponse;
    complete_run(CompleteRunRequest) -> CompleteRunResponse;
});

/// The call a request makes: with the key of its one `Authorization` header, or with none. No
/// value but a Bearer key is passed on (architecture.md, Wire).
fn call(headers: &HeaderMap) -> Call {
    let mut values = headers.get_all(AUTHORIZATION).iter();
    let Some(value) = values.next() else {
        return Call::anonymous();
    };
    let (None, Ok(text)) = (values.next(), value.to_str()) else {
        return malformed();
    };
    let (scheme, key) = text.split_once(' ').unwrap_or((text, ""));
    match key.trim_start_matches(' ') {
        _ if !scheme.eq_ignore_ascii_case("bearer") => malformed(),
        "" => Call::anonymous(),
        key => Call::with_key(Secret::new(key.to_owned())),
    }
}

/// A credential that is no Bearer key is a malformed key, which `Operations` answers as such.
fn malformed() -> Call {
    Call::with_key(Secret::new(wire::MALFORMED_KEY.to_owned()))
}
