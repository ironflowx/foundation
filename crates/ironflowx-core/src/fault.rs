//! The crash test's named fault points (#6, D23). The hooks are process-wide, so a point inside
//! open needs no `Ledger` to reach one, and in core so any crate's path can reach them.

use std::sync::OnceLock;

use crate::{EnvRef, KeyId, Secret};

/// A place in the commit, recovery, bootstrap or seed path where the crash test kills the process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultPoint {
    /// The writer has decided a record and not yet appended it.
    BeforeAppend,
    /// The record is durable and its submitter not yet answered.
    AfterFsync,
    /// The submitter is answered and the record not yet folded into the read store.
    AfterAck,
    /// Inside the writer's read store transaction for one record: its rows written, the
    /// applied position not yet moved, nothing committed.
    MidFold,
    /// The same place in open's recovery, folding a record the read store lacks.
    DuringOpen,
    /// Inside the transaction that gives a new control plane its schema.
    BeforeSchemaCommit,
    /// Inside the bootstrap's transaction, its rows written.
    BeforeBootstrapCommit,
    /// The bootstrap is durable and its ledger not begun.
    AfterBootstrapCommit,
    /// A new ledger's log is open and holds no record.
    BeforeGenesis,
    /// A new ledger's genesis record is durable and its read store not yet created.
    AfterGenesis,
    /// The ledger is built in staging and closed, and not yet at its final name.
    BeforeRename,
    /// The ledger is at its final name, and its rename not yet synced.
    AfterRename,
    /// Inside the transaction that marks the bootstrap, a bootstrapping or a finishing open's.
    BeforeMarkCommit,
    /// The mark is durable and `open` has not returned.
    AfterMarkCommit,
    /// A bootstrapping open has failed and undone nothing yet.
    BeforeUndo,
    /// Inside the undo's transaction: staging is gone and the rows' deletion not committed.
    BeforeUndoCommit,
    /// The undo is done, or has left a ledger in place and its rows, and `open` has not returned.
    AfterUndo,
    /// Inside the transaction that applies a new seed, the rotation written.
    BeforeSeedCommit,
    /// The new seed is durable and `open` has not returned.
    AfterSeedCommit,
}

impl FaultPoint {
    /// The name the crash test uses for this point.
    pub const fn name(self) -> &'static str {
        match self {
            Self::BeforeAppend => "before-append",
            Self::AfterFsync => "after-fsync",
            Self::AfterAck => "after-ack",
            Self::MidFold => "mid-fold",
            Self::DuringOpen => "during-open",
            Self::BeforeSchemaCommit => "before-schema-commit",
            Self::BeforeBootstrapCommit => "before-bootstrap-commit",
            Self::AfterBootstrapCommit => "after-bootstrap-commit",
            Self::BeforeGenesis => "before-genesis",
            Self::AfterGenesis => "after-genesis",
            Self::BeforeRename => "before-rename",
            Self::AfterRename => "after-rename",
            Self::BeforeMarkCommit => "before-mark-commit",
            Self::AfterMarkCommit => "after-mark-commit",
            Self::BeforeUndo => "before-undo",
            Self::BeforeUndoCommit => "before-undo-commit",
            Self::AfterUndo => "after-undo",
            Self::BeforeSeedCommit => "before-seed-commit",
            Self::AfterSeedCommit => "after-seed-commit",
        }
    }
}

static HOOK: OnceLock<fn(FaultPoint)> = OnceLock::new();

/// Calls `hook` on the thread that reaches each point, before the point's work. The
/// first hook set stays.
pub fn set_fault_hook(hook: fn(FaultPoint)) {
    let _ = HOOK.set(hook);
}

/// Calls the hook, if one is set, at `point`.
pub fn reach_fault_point(point: FaultPoint) {
    if let Some(hook) = HOOK.get() {
        hook(point);
    }
}

type Identity = fn(EnvRef, KeyId, Option<&Secret>);

static IDENTITY: OnceLock<Identity> = OnceLock::new();

/// Calls `hook` with each key an open is about to store, its environment, and its secret if the
/// engine minted it: the crash test learns which key to check before any point the key's storing
/// reaches. A finishing open's replacement is told before it is known to be needed. The first hook
/// set stays.
pub fn set_identity_hook(hook: Identity) {
    let _ = IDENTITY.set(hook);
}

/// Calls the hook, if one is set, with `key` of `env`.
pub fn tell_identity(env: EnvRef, key: KeyId, secret: Option<&Secret>) {
    if let Some(hook) = IDENTITY.get() {
        hook(env, key, secret);
    }
}
