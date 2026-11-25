//! The proto client: [`Operations`] over the network, through a door.

use std::fmt;
use std::net::Ipv6Addr;
use std::time::Duration;

use connectrpc::client::{
    BoxFuture, CallOptions, ClientBody, ClientConfig, ClientTransport, HttpClient, UnaryResponse,
};
use connectrpc::{CompressionRegistry, ConnectError, ErrorCode};
use http::header::{AUTHORIZATION, CONTENT_TYPE, HeaderValue};
use http::uri::{Scheme, Uri};
use http::{Request, Response, StatusCode};
use ironflowx_core::ErrorKind;

use crate::wire::{self, EnvironmentServiceClient, IdentityServiceClient, KeyServiceClient};
use crate::{
    Call, CreateKeyRequest, CreatedKey, EmitBatchRequest, EmitBatchResponse, EmitRequest,
    EmitResponse, Environment, Event, EventPage, GetEventRequest, KeyPage, ListEnvironmentsRequest,
    ListEventsRequest, ListKeysRequest, OpError, Operation, Operations, RevokeKeyRequest, Whoami,
    WhoamiRequest,
};

/// [`Operations`] over the network. It holds no key: each call sends its [`Call`]'s as the Bearer
/// key and an anonymous one sends none, so one client serves many keys. It accepts answers up to
/// 16 MiB. Calls need a tokio runtime.
///
/// It carries every operation but the events' four, which are `Internal` until their messages join
/// the proto.
pub struct Client {
    identity: IdentityServiceClient<Transport>,
    environments: EnvironmentServiceClient<Transport>,
    keys: KeyServiceClient<Transport>,
    timeout: Option<Duration>,
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client").finish_non_exhaustive()
    }
}

/// A URL no client calls: not `http://host[:port][/path]`. Names no part of it, since one may hold
/// a secret by mistake.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidUrl;

impl fmt::Display for InvalidUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("not a URL of the form http://host[:port][/path]")
    }
}

impl std::error::Error for InvalidUrl {}

impl Client {
    /// A client of the engine served at `url`; it connects at the first call.
    ///
    /// # Errors
    ///
    /// [`InvalidUrl`] if `url` is not `http://host[:port][/path]`: another scheme, a user, a query
    /// or a fragment.
    pub fn new(url: &str) -> Result<Self, InvalidUrl> {
        let uri: Uri = url.parse().map_err(|_| InvalidUrl)?;
        let authority = uri.authority().ok_or(InvalidUrl)?;
        let host = authority.host();
        // `Uri` takes any text in brackets, and any after the host.
        let address = match host.strip_prefix('[') {
            Some(v6) => v6.trim_end_matches(']').parse::<Ipv6Addr>().is_ok(),
            None => !host.is_empty(),
        };
        // A user comes before the host, or looks like it.
        let port = match authority.as_str().strip_prefix(host) {
            Some("") => true,
            Some(rest) => rest.strip_prefix(':').is_some_and(|port| {
                port.bytes().all(|b| b.is_ascii_digit()) && port.parse::<u16>().is_ok()
            }),
            None => false,
        };
        // `Uri` drops a fragment as it parses.
        let bare = !url.contains(['?', '#']);
        if uri.scheme() != Some(&Scheme::HTTP) || !(address && port && bare) {
            return Err(InvalidUrl);
        }
        let config = ClientConfig::new(uri)
            .with_default_max_message_size(wire::MAX_MESSAGE)
            // Another crate of the build may switch gzip or zstd on in `connectrpc`.
            .with_compression(CompressionRegistry::new());
        // Its clones share its connections.
        let http = Transport(HttpClient::plaintext());
        Ok(Self {
            identity: IdentityServiceClient::new(http.clone(), config.clone()),
            environments: EnvironmentServiceClient::new(http.clone(), config.clone()),
            keys: KeyServiceClient::new(http, config),
            timeout: None,
        })
    }

    /// This client, giving each call `timeout`: it ends by then, and the engine is told to stop. A
    /// new client gives a call no end.
    #[must_use]
    pub fn with_timeout(self, timeout: Duration) -> Self {
        Self {
            timeout: Some(timeout),
            ..self
        }
    }

    fn options(&self, cx: &Call) -> CallOptions {
        let options = with_key(cx);
        match self.timeout {
            Some(timeout) => options.with_timeout(timeout),
            None => options,
        }
    }
}

/// Passes on no success that is not Connect's: the library takes any 2xx, of any type that starts as
/// its own or of none, and an empty one decodes as an answer that says yes or lists nothing.
#[derive(Clone)]
struct Transport(HttpClient);

impl ClientTransport for Transport {
    type ResponseBody = <HttpClient as ClientTransport>::ResponseBody;
    type Error = ConnectError;

    fn send(
        &self,
        request: Request<ClientBody>,
    ) -> BoxFuture<'static, Result<Response<Self::ResponseBody>, ConnectError>> {
        let answer = self.0.send(request);
        Box::pin(async move {
            let answer = answer.await?;
            let kinds: Vec<_> = answer.headers().get_all(CONTENT_TYPE).iter().collect();
            if answer.status().is_success()
                && (answer.status() != StatusCode::OK || kinds != ["application/proto"])
            {
                let message = format!("an HTTP {} that is not Connect's", answer.status().as_u16());
                return Err(ConnectError::new(ErrorCode::Unknown, message));
            }
            Ok(answer)
        })
    }
}

/// Sends what `cx` holds: no header, or the key with the Bearer scheme.
fn with_key(cx: &Call) -> CallOptions {
    let Some(key) = cx.key() else {
        return CallOptions::default();
    };
    let key = key.expose();
    // A header cannot hold every text, and loses the spaces around one it can. No such key is
    // well formed, so send one that is not either: the engine answers both alike.
    let sent = if key.bytes().all(|b| b.is_ascii_graphic()) {
        key
    } else {
        wire::MALFORMED_KEY
    };
    let mut value =
        HeaderValue::from_str(&format!("Bearer {sent}")).expect("ASCII with no control");
    value.set_sensitive(true);
    CallOptions::default().with_header(AUTHORIZATION, value)
}

/// The answer of `op`, or why there is none. shortcut: without the engine's own error, a read is
/// `Unavailable` and a state change `Unknown`, also where the request cannot have run, as when the
/// connection never opened, or where the library's own code says more; tell them apart when a
/// caller needs to know.
fn answered<W, A>(op: Operation, answer: Result<W, ConnectError>) -> Result<A, OpError>
where
    A: TryFrom<W, Error = wire::Undecodable>,
{
    let failed = |message: String| OpError {
        kind: if op.changes_state() {
            ErrorKind::Unknown
        } else {
            ErrorKind::Unavailable
        },
        message,
        hint: match op {
            Operation::CreateKey => Some(
                "the key may be stored: list the keys before the retry, and again after the \
                 engine reopens; revoke the one no holder has"
                    .into(),
            ),
            Operation::RevokeKey => Some(
                "retry: revoking a revoked key succeeds, and one that revoked itself is refused"
                    .into(),
            ),
            _ => None,
        },
    };
    let answer = answer.map_err(|e| {
        wire::op_error(&e).unwrap_or_else(|| {
            let why = match op {
                // Its text can be the body of the answer, and of this one that holds the key
                // (TEN-3).
                Operation::CreateKey => e.code.as_str().to_owned(),
                _ => e.to_string(),
            };
            failed(format!("the engine did not answer: {why}"))
        })
    })?;
    answer
        .try_into()
        .map_err(|e: wire::Undecodable| failed(e.to_string()))
}

fn not_carried<T>(op: Operation) -> Result<T, OpError> {
    Err(OpError {
        kind: ErrorKind::Internal,
        message: format!("the client does not carry {}", op.rpc()),
        hint: None,
    })
}

impl Operations for Client {
    /// The calling key, as it sees itself.
    ///
    /// # Errors
    ///
    /// The engine's, unchanged; `Unavailable` if it did not answer.
    async fn whoami(&self, cx: &Call, req: WhoamiRequest) -> Result<Whoami, OpError> {
        let req = req.try_into()?;
        let options = self.options(cx);
        let answer = self.identity.whoami_with_options(req, options).await;
        answered(Operation::Whoami, answer.map(UnaryResponse::into_owned))
    }

    /// The environments the calling key reaches: only its own.
    ///
    /// # Errors
    ///
    /// As [`Client::whoami`].
    async fn list_environments(
        &self,
        cx: &Call,
        req: ListEnvironmentsRequest,
    ) -> Result<Vec<Environment>, OpError> {
        let req = req.try_into()?;
        let options = self.options(cx);
        let answer = self
            .environments
            .list_environments_with_options(req, options);
        let answer = answer.await.map(UnaryResponse::into_owned);
        answered(Operation::ListEnvironments, answer)
    }

    /// Creates a key in the calling key's environment; the answer holds its secret, once.
    ///
    /// # Errors
    ///
    /// The engine's, unchanged; `Unknown` if it did not answer; `Invalid`, with nothing sent, for
    /// an expiry outside years 0001–9999, which the wire does not carry.
    async fn create_key(&self, cx: &Call, req: CreateKeyRequest) -> Result<CreatedKey, OpError> {
        let req = req.try_into()?;
        let options = self.options(cx);
        let answer = self.keys.create_key_with_options(req, options).await;
        answered(Operation::CreateKey, answer.map(UnaryResponse::into_owned))
    }

    /// Lists a page of the calling key's environment's keys, newest first, never a secret.
    ///
    /// # Errors
    ///
    /// As [`Client::whoami`].
    async fn list_keys(&self, cx: &Call, req: ListKeysRequest) -> Result<KeyPage, OpError> {
        let req = req.try_into()?;
        let options = self.options(cx);
        let answer = self.keys.list_keys_with_options(req, options).await;
        answered(Operation::ListKeys, answer.map(UnaryResponse::into_owned))
    }

    /// Revokes a key of the calling key's environment; revoking a revoked key succeeds.
    ///
    /// # Errors
    ///
    /// The engine's, unchanged; `Unknown` if it did not answer.
    async fn revoke_key(&self, cx: &Call, req: RevokeKeyRequest) -> Result<(), OpError> {
        let req = req.try_into()?;
        let options = self.options(cx);
        let answer = self.keys.revoke_key_with_options(req, options).await;
        answered(Operation::RevokeKey, answer.map(UnaryResponse::into_owned))
    }

    async fn emit(&self, _cx: &Call, _req: EmitRequest) -> Result<EmitResponse, OpError> {
        not_carried(Operation::Emit)
    }

    async fn emit_batch(
        &self,
        _cx: &Call,
        _req: EmitBatchRequest,
    ) -> Result<EmitBatchResponse, OpError> {
        not_carried(Operation::EmitBatch)
    }

    async fn get_event(&self, _cx: &Call, _req: GetEventRequest) -> Result<Event, OpError> {
        not_carried(Operation::GetEvent)
    }

    async fn list_events(&self, _cx: &Call, _req: ListEventsRequest) -> Result<EventPage, OpError> {
        not_carried(Operation::ListEvents)
    }
}
