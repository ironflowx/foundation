//! The wire: the messages generated from the proto, and their mapping to the domain types. What
//! arrives is validated here, once, for the door and the client alike (D53).

use std::collections::BTreeSet;
use std::fmt;
use std::num::NonZeroU32;

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD};
use buffa::Message as _;
use buffa_types::google::protobuf::Timestamp as WireTime;
#[cfg(feature = "server")]
use connectrpc::ErrorDetail;
use connectrpc::{ConnectError, ErrorCode};
use ironflowx_core::{Address, ErrorKind, KeyOrigin, ParseError, Scope, Secret, Timestamp};

use crate::{KeyStatus, OpError};

mod generated {
    include!(concat!(env!("OUT_DIR"), "/wire.rs"));
}

pub use generated::ironflowx::v1::{
    CreateKeyRequest, CreateKeyResponse, Environment, ErrorInfo, KeyInfo, ListEnvironmentsRequest,
    ListEnvironmentsResponse, ListKeysRequest, ListKeysResponse, RevokeKeyRequest,
    RevokeKeyResponse, WhoamiRequest, WhoamiResponse,
};
#[cfg(feature = "server")]
pub use generated::ironflowx::v1::{
    EnvironmentService, EnvironmentServiceServer, IdentityService, IdentityServiceServer,
    KeyService, KeyServiceServer,
};
#[cfg(feature = "client")]
pub use generated::ironflowx::v1::{
    EnvironmentServiceClient, IdentityServiceClient, KeyServiceClient,
};

/// The compiled proto, as an encoded `FileDescriptorSet`: every file, its imports and their custom
/// options.
#[cfg(feature = "wire")]
pub const FILE_DESCRIPTOR_SET: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/descriptors.bin"));

/// The most a request body or a message holds, in the door and the client.
pub const MAX_MESSAGE: usize = 16 * 1024 * 1024;

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
            key_id: a.key_id.parse().map_err(|_| Undecodable)?,
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
                .map_err(|_| invalid("a scope is read, write or admin"))?,
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
            created_by: k.created_by.map(|id| id.to_string()),
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
        let created_by = k.created_by.map(|id| id.parse()).transpose();
        Ok(Self {
            key_id: k.key_id.parse().map_err(|_| Undecodable)?,
            label: k.label,
            scopes: read_scopes(&k.scopes).map_err(|_| Undecodable)?,
            origin: ORIGINS
                .into_iter()
                .find(|o| origin(*o) == k.origin)
                .ok_or(Undecodable)?,
            created_by: created_by.map_err(|_| Undecodable)?,
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
        let limit = r
            .limit
            .map(|limit| NonZeroU32::new(limit).ok_or_else(|| invalid("a limit is at least 1")));
        Ok(Self {
            limit: limit.transpose()?,
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

fn sent_scopes(scopes: &BTreeSet<Scope>) -> Vec<String> {
    scopes.iter().map(ToString::to_string).collect()
}

fn read_scopes(scopes: &[String]) -> Result<BTreeSet<Scope>, ParseError> {
    scopes.iter().map(|s| s.parse()).collect()
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
