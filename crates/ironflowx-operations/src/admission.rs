//! Admission: the one way to an [`Authorized`] (TEN-1). A call that carries this engine's
//! embedder grant is admitted as admin with no key (D121). Every other call runs, in order: no
//! key → `Unauthenticated`; `parse`; `lookup_key`, with no cache (D45); `verify` at the now it
//! read; `admit` against the operation's `required_scope` (D37). A key refused as newly
//! expired has its expiry recorded (`record_expiry`, D42), admission's one write.

use std::collections::BTreeSet;

use ironflowx_authz::{Grant, admit, parse, verify};
use ironflowx_core::{EnvRef, ErrorKind, KeyId, Scope, SubmitAs, Timestamp};
use ironflowx_proto::{Call, OpError, Operation, required_scope};

use crate::{Engine, blocking};

/// The embedding process's credential (D121): only [`Engine::embedder_call`] makes one, as no
/// other crate can name the type, and a door has no field to carry one:
///
/// ```compile_fail,E0603
/// # let (by, env) = todo!();
/// let _ = ironflowx_operations::EmbedderGrant { by, env };
/// ```
pub(crate) struct EmbedderGrant {
    pub(crate) by: SubmitAs,
    /// The environment and incarnation of the engine that made it, the only one it reaches.
    pub(crate) env: EnvRef,
}

/// Proof that a call was admitted: its key verified, or its grant this engine's own. Its field
/// is private to this module, so no handler can build one without admission, and the type is
/// private to the crate, so no other crate can name one:
///
/// ```compile_fail,E0603
/// fn hold(_: &ironflowx_operations::Authorized) {}
/// ```
pub(crate) struct Authorized {
    caller: Caller,
}

enum Caller {
    Key(Grant),
    /// Holds every scope and never expires.
    Embedder(SubmitAs, EnvRef),
}

impl Authorized {
    /// The calling key's ID; `None` for an embedder.
    pub(crate) const fn key_id(&self) -> Option<KeyId> {
        match &self.caller {
            Caller::Key(grant) => Some(grant.key_id()),
            Caller::Embedder(..) => None,
        }
    }

    pub(crate) fn scopes(&self) -> BTreeSet<Scope> {
        match &self.caller {
            Caller::Key(grant) => grant.scopes().clone(),
            // What a bootstrap key holds; tests/embedder.rs holds the two equal.
            Caller::Embedder(..) => BTreeSet::from([Scope::Read, Scope::Write, Scope::Admin]),
        }
    }

    pub(crate) const fn env(&self) -> EnvRef {
        match &self.caller {
            Caller::Key(grant) => grant.env(),
            Caller::Embedder(_, env) => *env,
        }
    }

    /// `None`: never.
    pub(crate) const fn expires_at(&self) -> Option<Timestamp> {
        match &self.caller {
            Caller::Key(grant) => grant.expires_at(),
            Caller::Embedder(..) => None,
        }
    }

    /// What a submission of this call is attributed to: its key, or `embedder:<name>` (TEN-6).
    pub(crate) fn submit_as(&self) -> SubmitAs {
        match &self.caller {
            Caller::Key(grant) => grant.submit_as(),
            Caller::Embedder(by, _) => by.clone(),
        }
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
        if let Some(grant) = cx.grant() {
            // Another engine's grant, of another directory or incarnation, reaches nothing here.
            return match grant.downcast_ref::<EmbedderGrant>() {
                Some(g) if g.env == self.env => Ok(Authorized {
                    caller: Caller::Embedder(g.by.clone(), g.env),
                }),
                _ => Err(unauthenticated("invalid grant", None)),
            };
        }
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
        Ok(Authorized {
            caller: Caller::Key(grant),
        })
    }
}
