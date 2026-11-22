//! Admission: the one way to an [`Authorized`] (TEN-1). Every call runs, in order: no key →
//! `Unauthenticated`; `parse`; `lookup_key`, with no cache (D45); `verify`.

use ironflowx_authz::{Grant, parse, verify};
use ironflowx_core::ErrorKind;
use ironflowx_proto::{Call, OpError};

use crate::{Engine, blocking};

/// Proof that a call's key was verified. Its field is private to this module, so no handler,
/// and no crate, can build one without admission:
///
/// ```compile_fail,E0451
/// # fn forge(grant: ironflowx_authz::Grant) -> ironflowx_operations::Authorized {
/// ironflowx_operations::Authorized { grant }
/// # }
/// ```
pub struct Authorized {
    grant: Grant,
}

impl Authorized {
    pub const fn grant(&self) -> &Grant {
        &self.grant
    }
}

fn unauthenticated(message: &str, hint: Option<String>) -> OpError {
    OpError {
        kind: ErrorKind::Unauthenticated,
        message: message.into(),
        hint,
    }
}

impl Engine {
    pub(crate) async fn admit(&self, cx: &Call) -> Result<Authorized, OpError> {
        let key = match cx.key() {
            Some(key) if !key.expose().is_empty() => key,
            _ => return Err(unauthenticated("authentication required", None)),
        };
        let parts =
            parse(key).map_err(|e| unauthenticated("invalid API key", Some(e.to_string())))?;
        let control = self.control.clone();
        let (id, env) = (parts.id, self.env);
        let record = blocking(move || control.lookup_key(id, &env)).await?;
        // One answer for every invalid key (D30).
        let grant = verify(&parts, record.as_ref(), &self.env)
            .map_err(|_| unauthenticated("invalid API key", None))?;
        Ok(Authorized { grant })
    }
}
