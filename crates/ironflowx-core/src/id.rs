use std::fmt;
use std::str::FromStr;

use crate::ParseError;

/// The identity of an environment: an opaque UUIDv7.
///
/// It names an environment but never addresses one alone; work takes an [`EnvRef`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EnvironmentId(Uuid7);

/// The identity of an event: an opaque UUIDv7.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EventId(Uuid7);

/// The identity of a run: an opaque UUIDv7.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RunId(Uuid7);

/// An execution's hold on its run: the run, and the execution's number, from 1. Text form:
/// `<run-id>.<execution>`, the number in decimal with no sign and no leading zero. A lease built
/// with execution 0, which no claim gives, prints text that does not parse back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Lease {
    pub run: RunId,
    pub execution: u32,
}

/// The identity of a tenant: an opaque UUIDv7.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TenantId(Uuid7);

/// The identity of a project: an opaque UUIDv7. A project stores its tenant's ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProjectId(Uuid7);

/// The identity of a key: an opaque UUIDv7. Not a secret.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyId(Uuid7);

/// A random 128-bit value minted when an environment is created. Text form: 32 lowercase hex
/// digits. Parsing that form is its only constructor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct IncarnationToken {
    bits: u128,
}

/// An environment ID with its incarnation token, always together.
/// Text form: `<uuid>.<32 hex digits>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EnvRef {
    id: EnvironmentId,
    token: IncarnationToken,
}

impl EnvRef {
    pub const fn id(&self) -> EnvironmentId {
        self.id
    }

    pub const fn token(&self) -> IncarnationToken {
        self.token
    }
}

/// A UUIDv7 that parsed from its canonical form: hyphenated, lowercase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct Uuid7(uuid::Uuid);

impl Uuid7 {
    fn parse(s: &str, what: &'static str) -> Result<Self, ParseError> {
        let err = ParseError::new(what);
        let u = uuid::Uuid::try_parse(s).map_err(|_| err.clone())?;
        let canonical = u
            .hyphenated()
            .encode_lower(&mut uuid::Uuid::encode_buffer())
            == s;
        let v7 = u.get_version() == Some(uuid::Version::SortRand)
            && u.get_variant() == uuid::Variant::RFC4122;
        if canonical && v7 {
            Ok(Self(u))
        } else {
            Err(err)
        }
    }
}

impl fmt::Display for Uuid7 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.hyphenated().fmt(f)
    }
}

impl FromStr for EnvironmentId {
    type Err = ParseError;
    fn from_str(s: &str) -> Result<Self, ParseError> {
        Uuid7::parse(s, "environment ID").map(Self)
    }
}

impl fmt::Display for EnvironmentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl FromStr for EventId {
    type Err = ParseError;
    fn from_str(s: &str) -> Result<Self, ParseError> {
        Uuid7::parse(s, "event ID").map(Self)
    }
}

impl fmt::Display for EventId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl FromStr for RunId {
    type Err = ParseError;
    fn from_str(s: &str) -> Result<Self, ParseError> {
        Uuid7::parse(s, "run ID").map(Self)
    }
}

impl fmt::Display for RunId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl FromStr for Lease {
    type Err = ParseError;
    fn from_str(s: &str) -> Result<Self, ParseError> {
        let err = || ParseError::new("lease");
        let (run, execution) = s.rsplit_once('.').ok_or_else(err)?;
        let execution: u32 = execution.parse().map_err(|_| err())?;
        // Only the form Display writes: from 1, no sign, no leading zero.
        if execution == 0 || s != format!("{run}.{execution}") {
            return Err(err());
        }
        Ok(Self {
            run: run.parse().map_err(|_| err())?,
            execution,
        })
    }
}

impl fmt::Display for Lease {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.run, self.execution)
    }
}

impl FromStr for TenantId {
    type Err = ParseError;
    fn from_str(s: &str) -> Result<Self, ParseError> {
        Uuid7::parse(s, "tenant ID").map(Self)
    }
}

impl fmt::Display for TenantId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl FromStr for ProjectId {
    type Err = ParseError;
    fn from_str(s: &str) -> Result<Self, ParseError> {
        Uuid7::parse(s, "project ID").map(Self)
    }
}

impl fmt::Display for ProjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl FromStr for KeyId {
    type Err = ParseError;
    fn from_str(s: &str) -> Result<Self, ParseError> {
        Uuid7::parse(s, "key ID").map(Self)
    }
}

impl fmt::Display for KeyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl FromStr for IncarnationToken {
    type Err = ParseError;
    fn from_str(s: &str) -> Result<Self, ParseError> {
        let err = ParseError::new("incarnation token");
        // from_str_radix alone would accept a sign and uppercase.
        if s.len() != 32 || !s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
            return Err(err);
        }
        u128::from_str_radix(s, 16)
            .map(|bits| Self { bits })
            .map_err(|_| err)
    }
}

impl fmt::Display for IncarnationToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:032x}", self.bits)
    }
}

impl FromStr for EnvRef {
    type Err = ParseError;
    fn from_str(s: &str) -> Result<Self, ParseError> {
        let (id, token) = s
            .split_once('.')
            .ok_or(ParseError::new("environment reference"))?;
        Ok(Self {
            id: id.parse()?,
            token: token.parse()?,
        })
    }
}

impl fmt::Display for EnvRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.id, self.token)
    }
}
