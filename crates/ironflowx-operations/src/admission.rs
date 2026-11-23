//! Admission: the one way to an [`Authorized`] (TEN-1). Every call runs, in order: no key →
//! `Unauthenticated`; `parse`; `lookup_key`, with no cache (D45); `verify` at the now it read;
//! `admit` against the operation's `required_scope` (D37). A key refused as newly
//! expired has its expiry recorded (`record_expiry`, D42), admission's one write.

use ironflowx_authz::{Grant, admit, parse, verify};
use ironflowx_core::ErrorKind;
use ironflowx_proto::{Call, OpError, Operation, required_scope};

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

pub(crate) fn unauthenticated(message: &str, hint: Option<String>) -> OpError {
    OpError {
        kind: ErrorKind::Unauthenticated,
        message: message.into(),
        hint,
    }
}

impl Engine {
    pub(crate) async fn admit(&self, cx: &Call, op: Operation) -> Result<Authorized, OpError> {
        let key = match cx.key() {
            Some(key) if !key.expose().is_empty() => key,
            _ => return Err(unauthenticated("authentication required", None)),
        };
        let parts =
            parse(key).map_err(|e| unauthenticated("invalid API key", Some(e.to_string())))?;
        let control = self.control.clone();
        let (id, env, clock) = (parts.id, self.env, self.clock.clone());
        let (record, now) = blocking(move || control.lookup_key(id, &env, &*clock)).await?;
        let grant = match verify(&parts, record.as_ref(), &self.env, now) {
            Ok(grant) => grant,
            Err(refused) => {
                if refused.newly_expired() {
                    let control = self.control.clone();
                    // If it fails, the next refusal tries again; the answer is the same (TEN-7).
                    let _ = blocking(move || control.record_expiry(&env, id)).await;
                }
                // One answer for every invalid key (D30).
                return Err(unauthenticated("invalid API key", None));
            }
        };
        admit(&grant, required_scope(op)).map_err(|missing| {
            let scope = missing.scope();
            OpError {
                kind: ErrorKind::PermissionDenied,
                message: format!("missing scope: {scope}"),
                hint: Some(format!(
                    "create a key with scope {scope}, or use one that has it"
                )),
            }
        })?;
        Ok(Authorized { grant })
    }
}
