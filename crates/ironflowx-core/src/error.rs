use std::fmt;

/// How a call failed. Every module and door reports failures with these kinds; the message
/// belongs to the module that produces it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorKind {
    /// A rule refused the request. The same request is refused the same way.
    Invalid,
    /// No such resource, or its ID does not parse.
    NotFound,
    /// The outcome is unknown; the ledger is now unavailable until reopened.
    Unknown,
    /// The ledger failed and refuses every call until it is reopened; or the control plane failed
    /// this call, which can be retried at once, unless a call has panicked under its lock: then
    /// only after a reopen.
    Unavailable,
    /// No key, or a key that is not valid for this environment.
    Unauthenticated,
    /// A valid key that lacks the operation's scope; the hint names it.
    PermissionDenied,
    /// A limit is reached; the hint says what frees room.
    ResourceExhausted,
}

impl ErrorKind {
    /// True when the same request, unchanged, may succeed later; the error's hint says when.
    pub const fn retryable(self) -> bool {
        matches!(
            self,
            Self::Unavailable | Self::Unknown | Self::ResourceExhausted
        )
    }
}

/// A text form that does not parse as the type it was parsed into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    what: &'static str,
}

impl ParseError {
    pub(crate) const fn new(what: &'static str) -> Self {
        Self { what }
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid {}", self.what)
    }
}

impl std::error::Error for ParseError {}
