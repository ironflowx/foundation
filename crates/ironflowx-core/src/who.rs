//! Attribution: who a submission is written as ([`SubmitAs`]) and who an event was emitted by,
//! as read back ([`Emitter`]).

use std::fmt;
use std::str::FromStr;

use crate::{KeyId, ParseError};

const MAX_EMBEDDER_BYTES: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Who {
    Key(KeyId),
    /// 1–128 bytes, no control, format or line or paragraph separator character.
    Embedder(String),
}

/// Who an event was emitted by, as read back. Text form: `key:<key-id>` or `embedder:<name>`.
///
/// Read-only: nothing turns an `Emitter` into a [`SubmitAs`] (D36).
///
/// ```compile_fail,E0277
/// # use ironflowx_core::{Emitter, SubmitAs};
/// fn resubmit(read: Emitter) -> SubmitAs { read.into() }
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Emitter(Who);

/// Who a submission is attributed to, as written. The ledger trusts its caller (D36).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SubmitAs(Who);

impl SubmitAs {
    /// An embedder that calls the ledger directly, without keys.
    ///
    /// # Errors
    ///
    /// If `name` is empty, over 128 bytes or holds a control, format or line or paragraph
    /// separator character.
    pub fn embedder(name: &str) -> Result<Self, ParseError> {
        embedder(name).map(Self)
    }

    /// A key's attribution. A clippy `disallowed-methods` entry bans this call outside the
    /// credentials crate, and `ci/no-key-attribution.sh` inside core (D17, D36).
    pub const fn key(id: KeyId) -> Self {
        Self(Who::Key(id))
    }
}

fn embedder(name: &str) -> Result<Who, ParseError> {
    let valid = (1..=MAX_EMBEDDER_BYTES).contains(&name.len()) && !name.chars().any(unreadable);
    valid
        .then(|| Who::Embedder(name.to_owned()))
        .ok_or(ParseError::new("embedder name"))
}

/// A character that hides or reorders what a reader of `emitted_by` sees: a control character
/// (Cc), a format character (Cf: bidi overrides, zero-width characters) or a line or paragraph
/// separator (Zl, Zp).
// shortcut: the Cf set is Unicode 16's, by hand, as core takes no Unicode-data dependency;
// recheck it when a Unicode release adds format characters.
fn unreadable(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{00AD}'
                | '\u{0600}'..='\u{0605}'
                | '\u{061C}'
                | '\u{06DD}'
                | '\u{070F}'
                | '\u{0890}'..='\u{0891}'
                | '\u{08E2}'
                | '\u{180E}'
                | '\u{200B}'..='\u{200F}'
                | '\u{2028}'..='\u{202E}'
                | '\u{2060}'..='\u{2064}'
                | '\u{2066}'..='\u{206F}'
                | '\u{FEFF}'
                | '\u{FFF9}'..='\u{FFFB}'
                | '\u{110BD}'
                | '\u{110CD}'
                | '\u{13430}'..='\u{1343F}'
                | '\u{1BCA0}'..='\u{1BCA3}'
                | '\u{1D173}'..='\u{1D17A}'
                | '\u{E0001}'
                | '\u{E0020}'..='\u{E007F}'
        )
}

impl From<SubmitAs> for Emitter {
    fn from(by: SubmitAs) -> Self {
        Self(by.0)
    }
}

impl FromStr for Emitter {
    type Err = ParseError;
    fn from_str(s: &str) -> Result<Self, ParseError> {
        let who = match s.split_once(':') {
            Some(("key", id)) => Who::Key(id.parse()?),
            Some(("embedder", name)) => embedder(name)?,
            _ => return Err(ParseError::new("emitter")),
        };
        Ok(Self(who))
    }
}

impl fmt::Display for Emitter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            Who::Key(id) => write!(f, "key:{id}"),
            Who::Embedder(name) => write!(f, "embedder:{name}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_attribution_reads_back_as_its_key() {
        let id: KeyId = "019a9f3e-7c2b-7d41-8a3e-5f6b7c8d9e0f".parse().unwrap();
        let emitter = Emitter::from(SubmitAs::key(id));
        assert_eq!(emitter.to_string(), format!("key:{id}"));
        assert_eq!(emitter.to_string().parse::<Emitter>(), Ok(emitter));
    }
}
