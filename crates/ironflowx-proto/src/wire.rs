//! The wire: the messages generated from the proto, and their mapping to the domain types. What
//! arrives is validated here, once, for the door and the client alike (D53).

use std::fmt;

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD};
use buffa::Message as _;
use buffa_types::google::protobuf::Timestamp as WireTime;
#[cfg(feature = "server")]
use connectrpc::ErrorDetail;
use connectrpc::{ConnectError, ErrorCode};
use ironflowx_core::{Address, ErrorKind, Timestamp};

use crate::OpError;

mod generated {
    include!(concat!(env!("OUT_DIR"), "/wire.rs"));
}

#[cfg(feature = "client")]
pub use generated::ironflowx::v1::IdentityServiceClient;
pub use generated::ironflowx::v1::{ErrorInfo, WhoamiRequest, WhoamiResponse};
#[cfg(feature = "server")]
pub use generated::ironflowx::v1::{IdentityService, IdentityServiceServer};

/// The most a request body or a message holds, in the door and the client.
pub const MAX_MESSAGE: usize = 16 * 1024 * 1024;

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
            key_id: a.key_id.to_string(),
            scopes: a.scopes.iter().map(ToString::to_string).collect(),
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
            key_id: a.key_id.parse().map_err(|_| Undecodable)?,
            scopes: a
                .scopes
                .iter()
                .map(|s| s.parse())
                .collect::<Result<_, _>>()
                .map_err(|_| Undecodable)?,
            environment: address(&a.environment)?,
            expires_at: a.expires_at.into_option().map(read_time).transpose()?,
        })
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

/// Every kind, with its text and its Connect code (architecture.md, Errors).
const KINDS: [ErrorKind; 8] = [
    ErrorKind::Invalid,
    ErrorKind::NotFound,
    ErrorKind::Unknown,
    ErrorKind::Unavailable,
    ErrorKind::Unauthenticated,
    ErrorKind::PermissionDenied,
    ErrorKind::ResourceExhausted,
    ErrorKind::Internal,
];

const fn kind(kind: ErrorKind) -> (&'static str, ErrorCode) {
    match kind {
        ErrorKind::Invalid => ("invalid", ErrorCode::InvalidArgument),
        ErrorKind::NotFound => ("not_found", ErrorCode::NotFound),
        ErrorKind::Unknown => ("unknown", ErrorCode::Unknown),
        ErrorKind::Unavailable => ("unavailable", ErrorCode::Unavailable),
        ErrorKind::Unauthenticated => ("unauthenticated", ErrorCode::Unauthenticated),
        ErrorKind::PermissionDenied => ("permission_denied", ErrorCode::PermissionDenied),
        ErrorKind::ResourceExhausted => ("resource_exhausted", ErrorCode::ResourceExhausted),
        ErrorKind::Internal => ("internal", ErrorCode::Internal),
    }
}

impl From<&OpError> for ErrorInfo {
    fn from(e: &OpError) -> Self {
        Self {
            kind: kind(e.kind).0.to_owned(),
            message: e.message.clone(),
            hint: e.hint.clone(),
            ..Self::default()
        }
    }
}

impl TryFrom<ErrorInfo> for OpError {
    type Error = Undecodable;

    fn try_from(info: ErrorInfo) -> Result<Self, Undecodable> {
        let of_text = |k: &ErrorKind| kind(*k).0 == info.kind;
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
    ConnectError::new(kind(e.kind).1, e.message.clone())
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
