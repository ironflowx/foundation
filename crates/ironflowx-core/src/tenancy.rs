//! Tenancy values: a key's scopes and an environment's address.

use std::fmt;
use std::str::FromStr;

use crate::ParseError;

/// What a key may do. A key holds a non-empty set; none implies another.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Scope {
    Read,
    Write,
    Admin,
}

impl Scope {
    /// Every scope: what a bootstrap key and an embedder's call hold.
    pub const ALL: [Self; 3] = [Self::Read, Self::Write, Self::Admin];
}

impl FromStr for Scope {
    type Err = ParseError;
    fn from_str(s: &str) -> Result<Self, ParseError> {
        match s {
            "read" => Ok(Self::Read),
            "write" => Ok(Self::Write),
            "admin" => Ok(Self::Admin),
            _ => Err(ParseError::new("scope")),
        }
    }
}

impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Admin => "admin",
        })
    }
}

/// How a key came to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyOrigin {
    /// The bootstrap key, minted by the engine.
    Minted,
    /// A seed the host supplied.
    Seed,
    /// An admin key recovered from the data directory.
    Recovered,
    /// Created by another key, which it names.
    Created,
}

/// Where an environment is, by name: `project/environment` while the single tenant is hidden.
///
/// Derived from names, never stored, and never parsed into an ID: it has no `FromStr`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Address {
    project: String,
    environment: String,
}

impl Address {
    pub fn new(project: &str, environment: &str) -> Self {
        Self {
            project: project.to_owned(),
            environment: environment.to_owned(),
        }
    }
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.project, self.environment)
    }
}
