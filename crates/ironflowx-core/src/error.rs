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
    /// A fault of the server, not of the request.
    Internal,
}

impl ErrorKind {
    /// True when the same request, sent again unchanged, may succeed and stores no second event;
    /// the error's hint says when. A kind that is not retryable may still have a hint that says
    /// what to do first.
    ///
    /// It promises no second event, not no second key: a `create_key` that answered `Unavailable`
    /// may have stored its key. List the keys before sending it again; a key whose commit is
    /// unsettled (D48) is listed only after a reopen, if it lands.
    pub const fn retryable(self) -> bool {
        matches!(self, Self::Unavailable)
    }

    /// The exit code of a process that ends with this kind. 0 is success.
    pub const fn exit_code(self) -> u8 {
        match self {
            Self::Internal => 1,
            Self::Invalid => 2,
            Self::Unauthenticated => 3,
            Self::PermissionDenied => 4,
            Self::NotFound => 5,
            Self::Unknown => 6,
            Self::ResourceExhausted => 7,
            Self::Unavailable => 8,
        }
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
