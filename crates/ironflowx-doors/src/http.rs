//! The HTTP door: Connect, gRPC and gRPC-Web on the same handlers, with `/health` and `/ready`.

use std::sync::Arc;

use axum::Router;
use axum::routing::get;
use connectrpc::{
    ConnectRpcService, Encodable, Limits, RequestContext, Response, ServiceRequest, ServiceResult,
};
use http::header::{AUTHORIZATION, CONTENT_TYPE};
use http::{HeaderMap, HeaderName, StatusCode};
use ironflowx_core::{ErrorKind, Secret};
use ironflowx_proto::wire::{
    self, IdentityService, IdentityServiceServer, WhoamiRequest, WhoamiResponse,
};
use ironflowx_proto::{Call, OpError, Operations};

use crate::{Readiness, Slot, WaitingOn};

/// The HTTP door of the engine in `slot`: its RPCs, `/health` and `/ready`.
pub fn router<O: Operations + 'static, R: Readiness + 'static>(slot: Arc<Slot<O, R>>) -> Router {
    let limits = Limits::default()
        .with_max_request_body_size(wire::MAX_MESSAGE)
        .with_max_message_size(wire::MAX_MESSAGE);
    let rpcs =
        ConnectRpcService::new(IdentityServiceServer::new(Door(slot.clone()))).with_limits(limits);
    Router::new()
        .route("/health", get(|| async { StatusCode::OK }))
        .route("/ready", get(move || async move { ready(&slot).await }))
        .fallback_service(rpcs)
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
    fn operations(&self) -> Result<&O, OpError> {
        match self.0.get() {
            Some((operations, _)) => Ok(operations),
            None => Err(OpError {
                kind: ErrorKind::Unavailable,
                message: "the engine is unavailable".into(),
                hint: Some("the engine is starting".into()),
            }),
        }
    }

    async fn ready(&self) -> Result<(), WaitingOn> {
        match self.0.get() {
            Some((_, readiness)) => readiness.ready().await,
            None => Err(WaitingOn::Open),
        }
    }
}

struct Door<O: Operations, R: Readiness>(Arc<Slot<O, R>>);

impl<O: Operations + 'static, R: Readiness + 'static> IdentityService for Door<O, R> {
    async fn whoami<'a>(
        &'a self,
        ctx: RequestContext,
        request: ServiceRequest<'_, WhoamiRequest>,
    ) -> ServiceResult<impl Encodable<WhoamiResponse> + Send + use<'a, O, R>> {
        let refused = |e| wire::connect_error(&e);
        let request = request.to_owned_message().try_into().map_err(refused)?;
        let operations = self.0.operations().map_err(refused)?;
        let answer = operations.whoami(&call(ctx.headers()), request).await;
        Response::ok(WhoamiResponse::from(answer.map_err(refused)?))
    }
}

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
