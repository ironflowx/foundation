//! The proto client: [`Operations`] over the network, through a door.

use std::error::Error;
use std::fmt;
use std::future::poll_fn;
use std::net::Ipv6Addr;
use std::pin::Pin;
use std::time::Duration;

use buffa::Message;
use connectrpc::client::{
    BoxFuture, CallOptions, ClientBody, ClientConfig, ClientTransport, HttpClient, UnaryResponse,
};
use connectrpc::http_body::Body as _;
use connectrpc::{CompressionRegistry, ConnectError, ErrorCode, ErrorDetail, JsonCodec};
use http::header::{AUTHORIZATION, CONTENT_ENCODING, CONTENT_TYPE, HeaderMap, HeaderValue};
use http::response::Parts;
use http::uri::{Scheme, Uri};
use http::{Request, Response, StatusCode};
use hyper_util::client::legacy;
use ironflowx_core::ErrorKind;
use serde::de::value::MapAccessDeserializer;
use serde::de::{IgnoredAny, MapAccess, Visitor};
use serde::{Deserialize, Deserializer};

use crate::wire::{
    self, EnvironmentServiceClient, EventServiceClient, IdentityServiceClient, KeyServiceClient,
};
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
/// # Errors
///
/// Of every call, first match wins (D56):
///
/// - `ResourceExhausted`, with nothing sent, for a request over 16 MiB; `Invalid` for one the wire
///   does not carry, as the call says.
/// - The engine's own error, unchanged, whatever the status and type of the failure it comes with.
/// - `Unavailable` if the connection never opened: nothing ran.
/// - What the server's library refused, as a Connect server sends it: `Invalid`,
///   `ResourceExhausted`, or `Internal` where the engine is older than this client and has no such
///   operation.
/// - Any other failure, as a drop, the timeout or a proxy's error: `Unavailable` of a read, and
///   `Unknown` of a call that changes state, which may have run; its hint says what to do.
pub struct Client {
    events: EventServiceClient<Transport>,
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

/// A URL no client calls: not `http://host[:port][/path]`, nor `https://` where the feature
/// `client-tls` is on. Names no part of it, since one may hold a secret by mistake.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidUrl;

impl fmt::Display for InvalidUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(if cfg!(feature = "client-tls") {
            "not a URL of the form http[s]://host[:port][/path]"
        } else {
            "not a URL of the form http://host[:port][/path]"
        })
    }
}

impl Error for InvalidUrl {}

impl Client {
    /// A client of the engine served at `url`; it connects at the first call. An `https` URL takes
    /// the feature `client-tls`, and reads here, once, the roots of the system's store.
    ///
    /// # Errors
    ///
    /// [`InvalidUrl`] if `url` is not `http://host[:port][/path]`, nor `https://` where
    /// `client-tls` is on: another scheme, a user, a query or a fragment.
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
        if !(address && port && bare) {
            return Err(InvalidUrl);
        }
        let http = match uri.scheme() {
            Some(s) if *s == Scheme::HTTP => HttpClient::plaintext(),
            #[cfg(feature = "client-tls")]
            Some(s) if *s == Scheme::HTTPS => HttpClient::with_tls(tls()),
            _ => return Err(InvalidUrl),
        };
        let config = ClientConfig::new(uri)
            .with_default_max_message_size(wire::MAX_MESSAGE)
            // Another crate of the build may switch gzip or zstd on in `connectrpc`.
            .with_compression(CompressionRegistry::new());
        // Its clones share its connections.
        let http = Transport(http);
        Ok(Self {
            events: EventServiceClient::new(http.clone(), config.clone()),
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

/// TLS that trusts the system's roots. Where there are none, no server is trusted.
#[cfg(feature = "client-tls")]
fn tls() -> std::sync::Arc<rustls::ClientConfig> {
    let mut roots = rustls::RootCertStore::empty();
    roots.add_parsable_certificates(rustls_native_certs::load_native_certs().certs);
    // Named, since another crate of the build may switch a second provider on in `rustls`, which
    // then has no default.
    let provider = rustls::crypto::aws_lc_rs::default_provider();
    let config = rustls::ClientConfig::builder_with_provider(provider.into())
        .with_safe_default_protocol_versions()
        .expect("the provider has the default versions");
    config
        .with_root_certificates(roots)
        .with_no_client_auth()
        .into()
}

/// Passes on no success that is not Connect's: the library takes any 2xx, of any type that starts
/// as its own or of none, and an empty one decodes as an answer that says yes or lists nothing.
/// Passes on no failure: the library gives a code to one that has none, and its own code to one it
/// cannot read, so [`Instead`] says what it was.
#[derive(Clone)]
struct Transport(HttpClient);

/// What came instead of an answer: the source of the error made from it.
#[derive(Debug)]
enum Instead {
    /// An error of Connect's as the server's library sends one, with no detail: JSON that names
    /// one of its codes, under that code's HTTP status. What sends all of that is taken for the
    /// library: nothing tells it apart.
    Refusal,
    /// As that, with details, so not the library's.
    Connect,
    /// None of Connect's.
    Http(StatusCode),
}

impl fmt::Display for Instead {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refusal | Self::Connect => f.write_str("an error of Connect's"),
            Self::Http(status) => {
                write!(f, "an HTTP {} that is not Connect's", status.as_u16())
            }
        }
    }
}

impl Error for Instead {}

type Incoming = <HttpClient as ClientTransport>::ResponseBody;

impl ClientTransport for Transport {
    type ResponseBody = Incoming;
    type Error = ConnectError;

    fn send(
        &self,
        request: Request<ClientBody>,
    ) -> BoxFuture<'static, Result<Response<Incoming>, ConnectError>> {
        let answer = self.0.send(request);
        Box::pin(async move {
            let (answer, body) = answer.await?.into_parts();
            if !answer.status.is_success() {
                return Err(failure(&answer, read(body).await.as_deref()));
            }
            let types: Vec<_> = answer.headers.get_all(CONTENT_TYPE).iter().collect();
            if answer.status == StatusCode::OK && types == ["application/proto"] {
                Ok(Response::from_parts(answer, body))
            } else {
                Err(failure(&answer, None))
            }
        })
    }
}

/// The whole of a body, unless it is cut short or is more than an answer holds.
async fn read(mut body: Incoming) -> Option<Vec<u8>> {
    let mut whole = Vec::new();
    while let Some(frame) = poll_fn(|cx| Pin::new(&mut body).poll_frame(cx)).await {
        if let Ok(data) = frame.ok()?.into_data() {
            if whole.len() + data.len() > wire::MAX_MESSAGE {
                return None;
            }
            whole.extend_from_slice(&data);
        }
    }
    Some(whole)
}

/// An answer, with the whole of its body if it was read, that is no success of Connect's.
fn failure(answer: &Parts, body: Option<&[u8]>) -> ConnectError {
    // Every request that can store an event or change a key holds a byte below a space, but a
    // `create_key` of nothing, and no error of a server of Connect's does, or begins otherwise: a
    // copy of a request in another's error is read for nothing.
    let object = body.filter(|body| body.starts_with(b"{") && body.iter().all(|b| *b >= b' '));
    let error = object.and_then(|body| JsonCodec::decode::<EngineError>(body).ok());
    let refusal = object
        .filter(|_| is_json(&answer.headers))
        .and_then(|body| JsonCodec::decode::<Refusal>(body).ok());
    let code = refusal.and_then(|refusal| {
        let code: ErrorCode = refusal.code.parse().ok()?;
        // As a server of Connect's sends it: each code under its own status, and a method it does
        // not have as not found.
        let missing = code == ErrorCode::Unimplemented && answer.status == StatusCode::NOT_FOUND;
        let alone = refusal.details.is_empty();
        (code.http_status() == answer.status || missing).then_some((code, alone))
    });
    let mut e = match code {
        Some((code, true)) => ConnectError::new(code, "").with_source(Instead::Refusal),
        Some((code, false)) => ConnectError::new(code, "").with_source(Instead::Connect),
        None => ConnectError::new(ErrorCode::Unknown, "").with_source(Instead::Http(answer.status)),
    };
    // They can hold the engine's own error, whatever a proxy made of the status and the type.
    let details = error.map_or_else(Vec::new, |error| error.details);
    e.details = details.into_iter().map(|detail| detail.0).collect();
    e
}

/// What the library's own refusal holds (architecture.md, Wire, rule 4).
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Refusal {
    code: String,
    // Not printed, but a text in what a server of Connect's sends.
    #[serde(default, rename = "message")]
    _message: String,
    // The library attaches none.
    #[serde(default)]
    details: Vec<IgnoredAny>,
}

/// What holds the engine's own error (rule 1).
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EngineError {
    #[serde(rename = "code")]
    _code: Option<String>,
    #[serde(rename = "message")]
    _message: Option<String>,
    #[serde(default)]
    details: Vec<Detail>,
}

/// A detail that is an object: serde takes an array for a struct too.
struct Detail(ErrorDetail);

impl<'de> Deserialize<'de> for Detail {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Object;

        impl<'de> Visitor<'de> for Object {
            type Value = Detail;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an object")
            }

            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Detail, A::Error> {
                ErrorDetail::deserialize(MapAccessDeserializer::new(map)).map(Detail)
            }
        }

        deserializer.deserialize_map(Object)
    }
}

/// Whether a body is JSON as it is: its one type says so, and it has no encoding.
fn is_json(headers: &HeaderMap) -> bool {
    let mut types = headers.get_all(CONTENT_TYPE).iter();
    let (Some(kind), None) = (types.next(), types.next()) else {
        return false;
    };
    // Whole, since what follows a first type can be another.
    let kind = kind.to_str().unwrap_or_default().to_ascii_lowercase();
    let json = match kind.split_once(';') {
        Some((kind, parameter)) => {
            let charset = matches!(parameter.trim(), "charset=utf-8" | r#"charset="utf-8""#);
            kind.trim_end() == "application/json" && charset
        }
        None => kind == "application/json",
    };
    json && !headers.contains_key(CONTENT_ENCODING)
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

/// `request`, unless it is over what a door takes. The door's own answer to that is often lost: it
/// closes the connection, as the rest is still sent.
fn fits<M: Message>(request: M) -> Result<M, OpError> {
    // None of a message too large to have a length.
    let len = request.try_encoded_len().ok();
    if len.is_some_and(|len| len as usize <= wire::MAX_MESSAGE) {
        return Ok(request);
    }
    Err(OpError {
        kind: ErrorKind::ResourceExhausted,
        message: "the request is over 16 MiB".into(),
        hint: None,
    })
}

const RETRY_KEYED: &str = "retry with the same idempotency key";
const RETRY_UNKEYED: &str = "check first what landed: a retry may store a second event";
const RETRY_BATCH: &str = "retry unchanged only if every item had an idempotency key and keeps it; \
                           else check first what landed";
const RETRY_CREATE: &str = "the key may be stored: list the keys before the retry, and again after \
                            the engine reopens; revoke the one no holder has";
const RETRY_REVOKE: &str =
    "retry: revoking a revoked key succeeds, and one that revoked itself is refused";

/// The answer of `op`, or why there is none (architecture.md, Wire): the engine's own error as it
/// is; else `Unavailable` where the call cannot have reached it, a code of the server's library as
/// the kind it means, and for any other failure `Unavailable` of a read and `Unknown`, with `hint`,
/// of a state change, which may have run.
fn answered<W, A>(
    op: Operation,
    hint: Option<&str>,
    answer: Result<W, ConnectError>,
) -> Result<A, OpError>
where
    A: TryFrom<W, Error = wire::Undecodable>,
{
    let may_have_run = |message: String| OpError {
        kind: if op.changes_state() {
            ErrorKind::Unknown
        } else {
            ErrorKind::Unavailable
        },
        message,
        hint: hint.map(str::to_owned),
    };
    let e = match answer {
        Ok(answer) => {
            let decoded = answer.try_into();
            return decoded.map_err(|e: wire::Undecodable| may_have_run(e.to_string()));
        }
        Err(e) => e,
    };
    if let Some(e) = wire::op_error(&e) {
        return Err(e);
    }
    let source = e.source();
    let instead = source.and_then(|s| s.downcast_ref::<Instead>());
    // Not its text, which can be the body of the answer: what answered may be no engine, and the
    // body can hold a key (TEN-3) or the data of events.
    let why = match instead {
        Some(instead @ Instead::Http(_)) => instead.to_string(),
        _ => e.code.as_str().to_owned(),
    };
    let refused = |kind, hint: Option<&str>| OpError {
        kind,
        message: format!("the call was refused: {why}"),
        hint: hint.map(str::to_owned),
    };
    if let Some(Instead::Refusal) = instead {
        match e.code {
            ErrorCode::InvalidArgument | ErrorCode::OutOfRange | ErrorCode::FailedPrecondition => {
                return Err(refused(ErrorKind::Invalid, None));
            }
            ErrorCode::ResourceExhausted => {
                return Err(refused(ErrorKind::ResourceExhausted, None));
            }
            ErrorCode::Unimplemented => {
                let hint = "the engine is older than this client";
                return Err(refused(ErrorKind::Internal, Some(hint)));
            }
            _ => {}
        }
    }
    let message = format!("the engine did not answer: {why}");
    let transport = source.and_then(|s| s.downcast_ref::<legacy::Error>());
    if transport.is_some_and(legacy::Error::is_connect) {
        return Err(OpError {
            kind: ErrorKind::Unavailable,
            message,
            hint: None,
        });
    }
    Err(may_have_run(message))
}

impl Operations for Client {
    /// Emits one event.
    ///
    /// # Errors
    ///
    /// As [`Client`]; `Invalid`, with nothing sent, for data or metadata that is not UTF-8, which
    /// the wire does not carry.
    async fn emit(&self, cx: &Call, req: EmitRequest) -> Result<EmitResponse, OpError> {
        let hint = match req.idempotency_key {
            Some(_) => RETRY_KEYED,
            None => RETRY_UNKEYED,
        };
        let req = fits(req.try_into()?)?;
        let options = self.options(cx);
        let answer = self.events.emit_with_options(req, options).await;
        answered(
            Operation::Emit,
            Some(hint),
            answer.map(UnaryResponse::into_owned),
        )
    }

    /// Emits a batch; the answer holds one result per input item, in input order.
    ///
    /// # Errors
    ///
    /// As [`Client::emit`]; one item the wire does not carry refuses the whole batch; and `Unknown`
    /// for an answer with another number of results than items.
    async fn emit_batch(
        &self,
        cx: &Call,
        req: EmitBatchRequest,
    ) -> Result<EmitBatchResponse, OpError> {
        let items = req.items.len();
        let req = fits(req.try_into()?)?;
        let options = self.options(cx);
        let answer = self.events.emit_batch_with_options(req, options).await;
        let answer = answer.map(UnaryResponse::into_owned).and_then(|a| {
            // An empty answer decodes as one to an empty batch.
            let whole = a.items.len() == items;
            whole
                .then_some(a)
                .ok_or_else(|| ConnectError::new(ErrorCode::Unknown, "not a result per item"))
        });
        answered(Operation::EmitBatch, Some(RETRY_BATCH), answer)
    }

    /// Reads one event by ID.
    ///
    /// # Errors
    ///
    /// As [`Client`].
    async fn get_event(&self, cx: &Call, req: GetEventRequest) -> Result<Event, OpError> {
        let req = fits(req.try_into()?)?;
        let options = self.options(cx);
        let answer = self.events.get_event_with_options(req, options).await;
        answered(
            Operation::GetEvent,
            None,
            answer.map(UnaryResponse::into_owned),
        )
    }

    /// Lists a page of events, newest first. A page shorter than its limit is not necessarily the
    /// last: only one without `next` is.
    ///
    /// # Errors
    ///
    /// As [`Client`].
    async fn list_events(&self, cx: &Call, req: ListEventsRequest) -> Result<EventPage, OpError> {
        let req = fits(req.try_into()?)?;
        let options = self.options(cx);
        let answer = self.events.list_events_with_options(req, options).await;
        answered(
            Operation::ListEvents,
            None,
            answer.map(UnaryResponse::into_owned),
        )
    }

    /// The calling key, as it sees itself.
    ///
    /// # Errors
    ///
    /// As [`Client`].
    async fn whoami(&self, cx: &Call, req: WhoamiRequest) -> Result<Whoami, OpError> {
        let req = req.try_into()?;
        let options = self.options(cx);
        let answer = self.identity.whoami_with_options(req, options).await;
        answered(
            Operation::Whoami,
            None,
            answer.map(UnaryResponse::into_owned),
        )
    }

    /// The environments the calling key reaches: only its own.
    ///
    /// # Errors
    ///
    /// As [`Client`].
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
        answered(Operation::ListEnvironments, None, answer)
    }

    /// Creates a key in the calling key's environment; the answer holds its secret, once.
    ///
    /// # Errors
    ///
    /// As [`Client`]; `Invalid`, with nothing sent, for an expiry outside years 0001–9999, which
    /// the wire does not carry.
    async fn create_key(&self, cx: &Call, req: CreateKeyRequest) -> Result<CreatedKey, OpError> {
        let req = fits(req.try_into()?)?;
        let options = self.options(cx);
        let answer = self.keys.create_key_with_options(req, options).await;
        answered(
            Operation::CreateKey,
            Some(RETRY_CREATE),
            answer.map(UnaryResponse::into_owned),
        )
    }

    /// Lists a page of the calling key's environment's keys, newest first, never a secret.
    ///
    /// # Errors
    ///
    /// As [`Client`].
    async fn list_keys(&self, cx: &Call, req: ListKeysRequest) -> Result<KeyPage, OpError> {
        let req = fits(req.try_into()?)?;
        let options = self.options(cx);
        let answer = self.keys.list_keys_with_options(req, options).await;
        answered(
            Operation::ListKeys,
            None,
            answer.map(UnaryResponse::into_owned),
        )
    }

    /// Revokes a key of the calling key's environment; revoking a revoked key succeeds.
    ///
    /// # Errors
    ///
    /// As [`Client`].
    async fn revoke_key(&self, cx: &Call, req: RevokeKeyRequest) -> Result<(), OpError> {
        let req = fits(req.try_into()?)?;
        let options = self.options(cx);
        let answer = self.keys.revoke_key_with_options(req, options).await;
        answered(
            Operation::RevokeKey,
            Some(RETRY_REVOKE),
            answer.map(UnaryResponse::into_owned),
        )
    }
}
