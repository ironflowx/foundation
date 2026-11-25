//! The proto client: [`Operations`] over the network, through a door.

use std::fmt;
use std::net::Ipv6Addr;

use connectrpc::client::{CallOptions, ClientConfig, HttpClient};
use connectrpc::{CompressionRegistry, ConnectError};
use http::header::{AUTHORIZATION, HeaderValue};
use http::uri::{Scheme, Uri};
use ironflowx_core::ErrorKind;

use crate::wire::{self, IdentityServiceClient};
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
/// It carries `whoami`; every other operation is `Internal` until its messages join the proto.
pub struct Client {
    identity: IdentityServiceClient<HttpClient>,
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
        Ok(Self {
            identity: IdentityServiceClient::new(HttpClient::plaintext(), config),
        })
    }
}

/// Sends what `cx` holds: no header, or the key with the Bearer scheme.
fn options(cx: &Call) -> CallOptions {
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

/// A read's failure. shortcut: without the engine's own error, any failure is `Unavailable`, which
/// is right for a read; an operation that changes state needs to say what it knows.
fn read_failed(e: &ConnectError) -> OpError {
    wire::op_error(e).unwrap_or_else(|| OpError {
        kind: ErrorKind::Unavailable,
        message: format!("the engine did not answer: {e}"),
        hint: None,
    })
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
        let answer = self.identity.whoami_with_options(req, options(cx)).await;
        let answer = answer.map_err(|e| read_failed(&e))?.into_owned();
        answer.try_into().map_err(|e: wire::Undecodable| OpError {
            kind: ErrorKind::Unavailable,
            message: e.to_string(),
            hint: None,
        })
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

    async fn list_environments(
        &self,
        _cx: &Call,
        _req: ListEnvironmentsRequest,
    ) -> Result<Vec<Environment>, OpError> {
        not_carried(Operation::ListEnvironments)
    }

    async fn create_key(&self, _cx: &Call, _req: CreateKeyRequest) -> Result<CreatedKey, OpError> {
        not_carried(Operation::CreateKey)
    }

    async fn list_keys(&self, _cx: &Call, _req: ListKeysRequest) -> Result<KeyPage, OpError> {
        not_carried(Operation::ListKeys)
    }

    async fn revoke_key(&self, _cx: &Call, _req: RevokeKeyRequest) -> Result<(), OpError> {
        not_carried(Operation::RevokeKey)
    }
}
