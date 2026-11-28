//! The Ironflow X control plane: which tenants, projects, environments and keys exist, in
//! `<root>/control.db` (SQLite).
//!
//! The engine uses this crate. Apps that open an engine use `ironflowx-operations`.
//!
//! Blocking: the engine's async methods call it under `spawn_blocking`. The bootstrap query and
//! admin-key recovery use their caller's thread. Every change is one transaction that takes
//! SQLite's write lock first (TEN-11); one that needs now then reads it from the clock it is given.
//!
//! One whose commit SQLite fails after writing it to the WAL is unsettled (D48): no open
//! connection sees it, but the next open of the file may find it landed, whole, unless a write
//! after it, or the last connection's close, clears it. Where an error below says nothing was
//! written, or no key changed, that is the one exception.

#![forbid(unsafe_code)]

use std::collections::BTreeSet;
use std::fmt;
use std::path::Path;
use std::sync::Mutex;

use ironflowx_authz::{
    CreateRefusal, Grant, KeyDigest, KeyParts, KeyRecord, SeedConflict, SeedState, may_create,
    may_create_as_embedder, recheck, verify_seed,
};
use ironflowx_core::{
    Clock, Emitter, EnvRef, KeyId, KeyOrigin, ProjectId, Scope, TenantId, Timestamp,
};
use rusqlite::{
    Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior, params,
};

/// The control plane's format. A change refuses older files at open.
const FORMAT: u16 = 1;
/// What bootstrap names the default tenant, project and environment.
const DEFAULT: &str = "default";
const MINTED: &str = "minted";
const SEED: &str = "seed";
const RECOVERED: &str = "recovered";
const CREATED: &str = "created";

const SCHEMA: &str = "
CREATE TABLE meta (format INTEGER NOT NULL);
CREATE TABLE tenants (id TEXT PRIMARY KEY, name TEXT NOT NULL, created_at INTEGER NOT NULL);
CREATE TABLE projects (
    id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL REFERENCES tenants(id),
    name TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE TABLE environments (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id),
    name TEXT NOT NULL,
    token TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    ledger_created_at INTEGER
);
CREATE TABLE keys (
    seq INTEGER PRIMARY KEY,
    id TEXT NOT NULL UNIQUE,
    environment_id TEXT NOT NULL REFERENCES environments(id),
    environment_token TEXT NOT NULL,
    scopes TEXT NOT NULL,
    digest BLOB NOT NULL,
    origin TEXT NOT NULL,
    created_by TEXT,
    label TEXT,
    expires_at INTEGER,
    expiry_recorded INTEGER NOT NULL DEFAULT 0,
    revoked_at INTEGER,
    created_at INTEGER NOT NULL
);
";

/// A control-plane read or write failed.
#[derive(Debug)]
pub enum StoreError {
    /// The file is in another format version.
    Format,
    /// SQLite failed, or a check of the stored rows did.
    Failed(String),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Format => f.write_str("control plane: another format version"),
            Self::Failed(why) => write!(f, "control plane: {why}"),
        }
    }
}

impl std::error::Error for StoreError {}

/// The private failure, so that `rusqlite` stays out of the public API.
enum Error {
    Db(rusqlite::Error),
    Format,
    Refused(&'static str),
}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Self::Db(e)
    }
}

const fn refused(why: &'static str) -> Error {
    Error::Refused(why)
}

/// Runs a body that fails with the private error, answering the public one.
fn run<T>(body: impl FnOnce() -> Result<T, Error>) -> Result<T, StoreError> {
    body().map_err(|e| match e {
        Error::Db(e) => StoreError::Failed(e.to_string()),
        Error::Format => StoreError::Format,
        Error::Refused(why) => StoreError::Failed(why.to_owned()),
    })
}

/// Where a data directory's bootstrap stands, read without creating anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootstrapState {
    NoFile,
    Unbootstrapped,
    /// Bootstrapped, but the ledger was not marked created.
    Unmarked {
        env: EnvRef,
    },
    Marked {
        env: EnvRef,
    },
}

/// What one bootstrap creates: the default tenant, project and environment, and its admin key.
/// The engine draws every ID and the key's secret, but for a seed's, which the host supplied.
#[derive(Debug, Clone)]
pub struct Bootstrap {
    pub env: EnvRef,
    pub tenant: TenantId,
    pub project: ProjectId,
    pub key: KeyParts,
    /// The key is a seed, else minted.
    pub by_seed: bool,
}

/// A key to create: the engine draws its ID and secret; its creation time, sequence number and
/// environment token are set inside the transaction.
#[derive(Debug, Clone)]
pub struct NewKey {
    pub id: KeyId,
    pub digest: KeyDigest,
    pub scopes: BTreeSet<Scope>,
    pub label: Option<String>,
    pub expires_at: Option<Timestamp>,
}

/// A stored key: what credentials need, and what is listed beside it.
#[derive(Debug, Clone)]
pub struct KeyRow {
    /// Assigned under the write lock, so two keys never tie.
    pub seq: u64,
    pub record: KeyRecord,
    pub origin: KeyOrigin,
    /// The key or embedder that created it; `None` unless its origin is `Created`.
    pub created_by: Option<Emitter>,
    pub label: Option<String>,
    pub created_at: Timestamp,
}

/// Why a key change did not happen. Nothing was written, but for a calling key found newly
/// expired, whose expiry it tries to record (D42), or for an unsettled commit (D48).
/// Its `Display` shows the inner error, and it has no `source()`, so a reporter that walks the
/// chain shows the cause once (D84).
#[derive(Debug)]
pub enum KeyChangeError {
    /// The calling key is unknown, revoked, expired, or of another environment, read again
    /// inside the transaction (D43).
    Caller,
    /// A creation request breaks a creation rule.
    Refused(CreateRefusal),
    /// As many created keys as the cap allows hold a slot.
    Full,
    /// No key has that ID in this environment.
    NotFound,
    Store(StoreError),
}

impl fmt::Display for KeyChangeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Caller => f.write_str("the calling key is no longer valid"),
            Self::Refused(r) => r.fmt(f),
            Self::Full => f.write_str("key limit reached"),
            Self::NotFound => f.write_str("key not found"),
            Self::Store(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for KeyChangeError {}

/// Why a seed was not applied. No key changed, but for an unsettled commit, if `Store` (D48).
/// Its `Display` shows the inner error, and it has no `source()`, so a reporter that walks the
/// chain shows the cause once (D84).
#[derive(Debug)]
pub enum SeedError {
    /// The seed's key ID is already a key, and not this seed.
    Conflict,
    Store(StoreError),
}

impl fmt::Display for SeedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Conflict => SeedConflict.fmt(f),
            Self::Store(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for SeedError {}

/// Why no key was recovered. No key changed, but for an unsettled commit, if `Store` (D48).
/// Its `Display` shows the inner error, and it has no `source()`, so a reporter that walks the
/// chain shows the cause once (D84).
#[derive(Debug)]
pub enum RecoverError {
    /// The bootstrap is not finished: there is none, or its ledger was not marked created.
    NotReady,
    Store(StoreError),
}

impl fmt::Display for RecoverError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotReady => f.write_str("the bootstrap is not finished"),
            Self::Store(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for RecoverError {}

impl From<StoreError> for RecoverError {
    fn from(e: StoreError) -> Self {
        Self::Store(e)
    }
}

/// An environment with its parents' IDs and the names its address is derived from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvironmentRecord {
    pub env: EnvRef,
    pub name: String,
    pub project_id: ProjectId,
    pub project_name: String,
    pub tenant_id: TenantId,
    pub created_at: Timestamp,
}

/// The control plane: one connection behind a mutex.
pub struct ControlPlane {
    conn: Mutex<Connection>,
}

impl ControlPlane {
    /// Opens `path`, creating it with an empty schema if missing.
    ///
    /// # Errors
    ///
    /// `Format` if the file is in another format; `Failed` if it is no control plane, or SQLite
    /// fails.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        run(|| {
            let mut conn = Connection::open(path)?;
            // Before the journal mode, which is written to the file.
            has_schema(&conn)?;
            conn.pragma_update(None, "journal_mode", "WAL")?;
            conn.pragma_update(None, "synchronous", "FULL")?;
            // A no-op except on macOS, where fsync alone does not reach the disk.
            conn.pragma_update(None, "fullfsync", true)?;
            conn.pragma_update(None, "foreign_keys", true)?;
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            if !has_schema(&tx)? {
                tx.execute_batch(SCHEMA)?;
                tx.execute("INSERT INTO meta VALUES (?1)", [FORMAT])?;
                #[cfg(feature = "__fault-points")]
                ironflowx_core::reach_fault_point(ironflowx_core::FaultPoint::BeforeSchemaCommit);
            }
            tx.commit()?;
            Ok(Self {
                conn: Mutex::new(conn),
            })
        })
    }

    /// Reads where the bootstrap at `path` stands. Writes no row, creates nothing, and leaves
    /// behind no file SQLite makes to read it but `-shm`, which it derives from a WAL a crash left.
    ///
    /// # Errors
    ///
    /// `Format` if the file is in another format; `Failed` if it is no control plane, or SQLite
    /// fails.
    pub fn bootstrap_state(path: &Path) -> Result<BootstrapState, StoreError> {
        run(|| {
            if !path
                .try_exists()
                .map_err(|_| refused("cannot read the data directory"))?
            {
                return Ok(BootstrapState::NoFile);
            }
            let conn = open_to_read(path)?;
            // A crash before the schema's transaction committed.
            if !has_schema(&conn)? {
                return Ok(BootstrapState::Unbootstrapped);
            }
            read_state(&conn)
        })
    }

    /// Creates the default tenant, project and environment and the admin key, minted or a seed,
    /// in one transaction.
    ///
    /// # Errors
    ///
    /// If the file is already bootstrapped, or SQLite fails; nothing is written then, but for an
    /// unsettled commit (D48).
    pub fn ensure_bootstrap(&self, b: &Bootstrap, clock: &dyn Clock) -> Result<(), StoreError> {
        run(|| {
            let mut conn = self.lock()?;
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let at = clock.now();
            let now = at.as_millis();
            let bootstrapped: bool =
                tx.query_row("SELECT count(*) > 0 FROM environments", [], |r| r.get(0))?;
            if bootstrapped {
                return Err(refused("already bootstrapped"));
            }
            let (tenant, project, env) = (b.tenant.to_string(), b.project.to_string(), b.env);
            tx.execute(
                "INSERT INTO tenants VALUES (?1, ?2, ?3)",
                params![tenant, DEFAULT, now],
            )?;
            tx.execute(
                "INSERT INTO projects VALUES (?1, ?2, ?3, ?4)",
                params![project, tenant, DEFAULT, now],
            )?;
            tx.execute(
                "INSERT INTO environments VALUES (?1, ?2, ?3, ?4, ?5, NULL)",
                params![
                    env.id().to_string(),
                    project,
                    DEFAULT,
                    env.token().to_string(),
                    now
                ],
            )?;
            let origin = if b.by_seed { SEED } else { MINTED };
            insert_admin(&tx, &b.key, &env, origin, None, at)?;
            #[cfg(feature = "__fault-points")]
            ironflowx_core::reach_fault_point(ironflowx_core::FaultPoint::BeforeBootstrapCommit);
            tx.commit()?;
            #[cfg(feature = "__fault-points")]
            ironflowx_core::reach_fault_point(ironflowx_core::FaultPoint::AfterBootstrapCommit);
            Ok(())
        })
    }

    /// Deletes everything the bootstrap of `env` created, in one transaction; a file that does not
    /// hold `env` is left as it is.
    ///
    /// # Errors
    ///
    /// If `env` is marked, or SQLite fails; nothing is deleted then, but for an unsettled commit
    /// (D48).
    pub fn undo_bootstrap(&self, env: &EnvRef) -> Result<(), StoreError> {
        run(|| {
            let mut conn = self.lock()?;
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let (id, token) = (env.id().to_string(), env.token().to_string());
            let row = tx
                .query_row(
                    "SELECT e.ledger_created_at IS NOT NULL, p.id, p.tenant_id \
                     FROM environments e JOIN projects p ON p.id = e.project_id \
                     WHERE e.id = ?1 AND e.token = ?2",
                    params![id, token],
                    |r| {
                        Ok((
                            r.get::<_, bool>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, String>(2)?,
                        ))
                    },
                )
                .optional()?;
            let Some((marked, project, tenant)) = row else {
                return Ok(());
            };
            if marked {
                return Err(refused("the bootstrap is finished"));
            }
            tx.execute("DELETE FROM keys WHERE environment_id = ?1", [&id])?;
            tx.execute("DELETE FROM environments WHERE id = ?1", [&id])?;
            tx.execute("DELETE FROM projects WHERE id = ?1", [project])?;
            tx.execute("DELETE FROM tenants WHERE id = ?1", [tenant])?;
            #[cfg(feature = "__fault-points")]
            ironflowx_core::reach_fault_point(ironflowx_core::FaultPoint::BeforeUndoCommit);
            tx.commit()?;
            Ok(())
        })
    }

    /// Marks `env`'s ledger created: the bootstrap is finished.
    ///
    /// # Errors
    ///
    /// If `env` is not this file's environment or is marked already, or SQLite fails; nothing is
    /// written then, but for an unsettled commit (D48).
    pub fn mark_ledger_created(&self, env: &EnvRef, clock: &dyn Clock) -> Result<(), StoreError> {
        run(|| {
            let mut conn = self.lock()?;
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            mark(&tx, env, clock.now())?;
            #[cfg(feature = "__fault-points")]
            ironflowx_core::reach_fault_point(ironflowx_core::FaultPoint::BeforeMarkCommit);
            tx.commit()?;
            #[cfg(feature = "__fault-points")]
            ironflowx_core::reach_fault_point(ironflowx_core::FaultPoint::AfterMarkCommit);
            Ok(())
        })
    }

    /// Marks the bootstrap of `env` that an earlier open left unfinished, in one transaction: a
    /// minted key, whose secret no open returned, is revoked, and `replacement`, if given,
    /// inserted in its place. Whether there was one to revoke.
    ///
    /// # Errors
    ///
    /// As [`ControlPlane::mark_ledger_created`].
    pub fn finish_bootstrap(
        &self,
        env: &EnvRef,
        replacement: Option<&KeyParts>,
        clock: &dyn Clock,
    ) -> Result<bool, StoreError> {
        run(|| {
            let mut conn = self.lock()?;
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let now = clock.now();
            mark(&tx, env, now)?;
            let lost = revoke_all(&tx, env, MINTED, now)? > 0;
            if lost && let Some(key) = replacement {
                insert_admin(&tx, key, env, MINTED, None, now)?;
            }
            #[cfg(feature = "__fault-points")]
            ironflowx_core::reach_fault_point(ironflowx_core::FaultPoint::BeforeMarkCommit);
            tx.commit()?;
            #[cfg(feature = "__fault-points")]
            ironflowx_core::reach_fault_point(ironflowx_core::FaultPoint::AfterMarkCommit);
            Ok(lost)
        })
    }

    /// Reads whether [`ControlPlane::apply_seed`] would find `seed` in conflict with a key of
    /// `env` at `path`; writes nothing, as [`ControlPlane::bootstrap_state`].
    ///
    /// # Errors
    ///
    /// As [`ControlPlane::apply_seed`].
    pub fn check_seed(path: &Path, env: &EnvRef, seed: &KeyParts) -> Result<(), SeedError> {
        run(|| Ok(seed_state(&open_to_read(path)?, env, seed)?.map(drop)))
            .map_err(SeedError::Store)?
            .map_err(|SeedConflict| SeedError::Conflict)
    }

    /// Applies the seed a host supplied to `env`, in one transaction: a key ID not stored yet
    /// revokes the current seed, if any, and becomes the current one, other keys unchanged; the
    /// current seed, or a revoked one, changes nothing (D33).
    ///
    /// # Errors
    ///
    /// `Conflict` if the seed's key ID is already a key and not this seed, `Store` if `env` is
    /// not this file's environment or SQLite fails; no key changes, but for an unsettled commit
    /// (D48).
    pub fn apply_seed(
        &self,
        env: &EnvRef,
        seed: &KeyParts,
        clock: &dyn Clock,
    ) -> Result<SeedState, SeedError> {
        // The outer error is SQLite's; the inner one, a check that refused.
        run(|| {
            let mut conn = self.lock()?;
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let now = clock.now();
            match seed_state(&tx, env, seed)? {
                Ok(Some(state)) => return Ok(Ok(state)),
                Err(conflict) => return Ok(Err(conflict)),
                Ok(None) => {}
            }
            #[cfg(feature = "__fault-points")]
            ironflowx_core::tell_key(*env, seed.id, None);
            revoke_all(&tx, env, SEED, now)?;
            insert_admin(&tx, seed, env, SEED, None, now)?;
            #[cfg(feature = "__fault-points")]
            ironflowx_core::reach_fault_point(ironflowx_core::FaultPoint::BeforeSeedCommit);
            tx.commit()?;
            #[cfg(feature = "__fault-points")]
            ironflowx_core::reach_fault_point(ironflowx_core::FaultPoint::AfterSeedCommit);
            Ok(Ok(SeedState::Current))
        })
        .map_err(SeedError::Store)?
        .map_err(|SeedConflict| SeedError::Conflict)
    }

    /// Inserts `key` as an admin key of the file's environment, of origin `Recovered` and labelled
    /// `recovered`, in one transaction that first checks the bootstrap is marked: only an unmarked
    /// one is ever undone, so a recovered key never is (D34, TEN-14).
    ///
    /// # Errors
    ///
    /// `NotReady` unless the bootstrap is marked, `Store` if SQLite fails; no key is inserted, but
    /// for an unsettled commit (D48).
    pub fn insert_recovered(&self, key: &KeyParts, clock: &dyn Clock) -> Result<(), RecoverError> {
        // The outer error is SQLite's; the inner one, a check that refused.
        run(|| {
            let mut conn = self.lock()?;
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let now = clock.now();
            let BootstrapState::Marked { env } = read_state(&tx)? else {
                return Ok(Err(RecoverError::NotReady));
            };
            insert_admin(&tx, key, &env, RECOVERED, Some(RECOVERED), now)?;
            #[cfg(feature = "__fault-points")]
            ironflowx_core::reach_fault_point(ironflowx_core::FaultPoint::BeforeRecoveredCommit);
            tx.commit()?;
            #[cfg(feature = "__fault-points")]
            ironflowx_core::reach_fault_point(ironflowx_core::FaultPoint::AfterRecoveredCommit);
            Ok(Ok(()))
        })?
    }

    /// The key `id`, if it was made for `env`'s ID and incarnation token, and then `clock`'s now:
    /// admission's one read. One step, so no expiry this control plane records falls between
    /// them, and a key is never judged by an old row and a clock set back since (TEN-13).
    ///
    /// # Errors
    ///
    /// If SQLite fails, or the stored row does not decode.
    pub fn lookup_key(
        &self,
        id: KeyId,
        env: &EnvRef,
        clock: &dyn Clock,
    ) -> Result<(Option<KeyRecord>, Timestamp), StoreError> {
        run(|| {
            let conn = self.lock()?;
            let record = lookup(&conn, id, env)?;
            Ok((record, clock.now()))
        })
    }

    /// Records that `id`, a key of `env` that is not revoked, was refused as expired: it stays
    /// expired whatever the clock does (D42). Recording it again changes nothing.
    ///
    /// # Errors
    ///
    /// If SQLite fails; nothing is recorded then, but for an unsettled commit (D48).
    pub fn record_expiry(&self, env: &EnvRef, id: KeyId) -> Result<(), StoreError> {
        run(|| {
            let mut conn = self.lock()?;
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            set_expiry_recorded(&tx, id, env)?;
            Ok(commit_expiry(tx)?)
        })
    }

    /// Creates `new` in `env` for `creator`, a key or the embedding process, in one transaction:
    /// with the write lock held and now read from `clock`, a creating key's record is read again
    /// and checked (`recheck`), then the request (`may_create`, or `may_create_as_embedder`, as no
    /// record backs an embedder, D121), so no key outlives or outranks its creator (TEN-5), then
    /// that fewer than `cap` created keys hold a slot: one does until it is revoked or its
    /// expiry is recorded, whatever the clock reads (TEN-15).
    ///
    /// # Errors
    ///
    /// `Caller`, `Refused` or `Full` as the checks find, `Store` if SQLite fails; no key is
    /// created, but for an unsettled commit (D48). A newly expired `creator` has its expiry
    /// recorded, unless that fails.
    pub fn insert_key(
        &self,
        env: &EnvRef,
        new: NewKey,
        creator: &Emitter,
        clock: &dyn Clock,
        cap: u32,
    ) -> Result<KeyRow, KeyChangeError> {
        // The outer error is SQLite's; the inner one, a check that refused.
        run(|| {
            let mut conn = self.lock()?;
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let now = clock.now();
            let Some((tx, grant)) = changer(tx, creator, env, now)? else {
                return Ok(Err(KeyChangeError::Caller));
            };
            let (scopes, label) = (&new.scopes, new.label.as_deref());
            let allowed = match &grant {
                Some(grant) => may_create(grant, scopes, new.expires_at, label, now),
                None => may_create_as_embedder(scopes, new.expires_at, label, now),
            };
            if let Err(r) = allowed {
                return Ok(Err(KeyChangeError::Refused(r)));
            }
            let slots: u32 = tx.query_row(
                "SELECT count(*) FROM keys WHERE environment_id = ?1 AND environment_token = ?2 \
                 AND origin = ?3 AND revoked_at IS NULL AND expiry_recorded = 0",
                params![env.id().to_string(), env.token().to_string(), CREATED],
                |r| r.get(0),
            )?;
            if slots >= cap {
                return Ok(Err(KeyChangeError::Full));
            }
            tx.execute(
                "INSERT INTO keys (id, environment_id, environment_token, scopes, digest, origin, \
                 created_by, label, expires_at, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    new.id.to_string(),
                    env.id().to_string(),
                    env.token().to_string(),
                    scopes_text(&new.scopes),
                    new.digest.to_stored(),
                    CREATED,
                    stored_creator(creator),
                    label,
                    new.expires_at.map(Timestamp::as_millis),
                    now.as_millis()
                ],
            )?;
            let seq = u64::try_from(tx.last_insert_rowid()).map_err(|_| refused("bad seq"))?;
            #[cfg(feature = "__fault-points")]
            ironflowx_core::reach_fault_point(ironflowx_core::FaultPoint::BeforeCreateCommit);
            tx.commit()?;
            #[cfg(feature = "__fault-points")]
            ironflowx_core::reach_fault_point(ironflowx_core::FaultPoint::AfterCreateCommit);
            Ok(Ok(KeyRow {
                seq,
                record: KeyRecord {
                    id: new.id,
                    env: *env,
                    scopes: new.scopes,
                    digest: new.digest,
                    expires_at: new.expires_at,
                    expiry_recorded: false,
                    revoked_at: None,
                },
                origin: KeyOrigin::Created,
                created_by: Some(creator.clone()),
                label: new.label,
                created_at: now,
            }))
        })
        .map_err(KeyChangeError::Store)?
    }

    /// Revokes `id` in `env` for `by`, a key or the embedding process, in one transaction: with
    /// the write lock held and now read from `clock`, a revoking key's record is read again and
    /// checked (`recheck`, D43), then `id`'s `revoked_at` is set unless it already is. The row
    /// stays (D35).
    ///
    /// # Errors
    ///
    /// `Caller` if `by` is no longer valid, `NotFound` if `env` holds no key `id`, `Store` if
    /// SQLite fails; no key is revoked, but for an unsettled commit (D48). A newly expired `by`
    /// has its expiry recorded, unless that fails.
    pub fn revoke_key(
        &self,
        env: &EnvRef,
        id: KeyId,
        by: &Emitter,
        clock: &dyn Clock,
    ) -> Result<(), KeyChangeError> {
        // The outer error is SQLite's; the inner one, a check that refused.
        run(|| {
            let mut conn = self.lock()?;
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let now = clock.now();
            let Some((tx, _)) = changer(tx, by, env, now)? else {
                return Ok(Err(KeyChangeError::Caller));
            };
            if lookup(&tx, id, env)?.is_none() {
                return Ok(Err(KeyChangeError::NotFound));
            }
            tx.execute(
                "UPDATE keys SET revoked_at = ?1 WHERE id = ?2 AND revoked_at IS NULL",
                params![now.as_millis(), id.to_string()],
            )?;
            #[cfg(feature = "__fault-points")]
            ironflowx_core::reach_fault_point(ironflowx_core::FaultPoint::BeforeRevokeCommit);
            tx.commit()?;
            #[cfg(feature = "__fault-points")]
            ironflowx_core::reach_fault_point(ironflowx_core::FaultPoint::AfterRevokeCommit);
            Ok(Ok(()))
        })
        .map_err(KeyChangeError::Store)?
    }

    /// Up to `limit` of `env`'s keys, newest first by `seq`, those before `before_seq` only
    /// when it is given.
    ///
    /// # Errors
    ///
    /// If SQLite fails, or a stored row does not decode.
    pub fn list_keys(
        &self,
        env: &EnvRef,
        before_seq: Option<u64>,
        limit: u32,
    ) -> Result<Vec<KeyRow>, StoreError> {
        run(|| {
            let conn = self.lock()?;
            let mut stmt = conn.prepare(
                "SELECT seq, id, scopes, digest, expires_at, expiry_recorded, revoked_at, origin, \
                 created_by, label, created_at FROM keys \
                 WHERE environment_id = ?1 AND environment_token = ?2 AND seq < ?3 \
                 ORDER BY seq DESC LIMIT ?4",
            )?;
            let before = before_seq.map_or(i64::MAX, |s| i64::try_from(s).unwrap_or(i64::MAX));
            let rows = stmt.query_map(
                params![env.id().to_string(), env.token().to_string(), before, limit],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        Stored {
                            scopes: r.get(2)?,
                            digest: r.get(3)?,
                            expires_at: r.get(4)?,
                            expiry_recorded: r.get(5)?,
                            revoked_at: r.get(6)?,
                        },
                        r.get::<_, String>(7)?,
                        r.get::<_, Option<String>>(8)?,
                        r.get::<_, Option<String>>(9)?,
                        r.get::<_, i64>(10)?,
                    ))
                },
            )?;
            rows.map(|row| {
                let (seq, id, stored, origin, created_by, label, created_at) = row?;
                let id = id.parse().map_err(|_| refused("bad key ID"))?;
                Ok(KeyRow {
                    seq: u64::try_from(seq).map_err(|_| refused("bad seq"))?,
                    record: stored.record(id, env)?,
                    origin: origin_from(&origin)?,
                    created_by: created_by.map(|c| creator_from(&c)).transpose()?,
                    label,
                    created_at: Timestamp::from_millis(created_at),
                })
            })
            .collect()
        })
    }

    /// The environment `env` with its parents.
    ///
    /// # Errors
    ///
    /// If there is no such environment, or SQLite fails.
    pub fn environment(&self, env: &EnvRef) -> Result<EnvironmentRecord, StoreError> {
        run(|| {
            let conn = self.lock()?;
            let row = conn
                .query_row(
                    "SELECT e.name, e.created_at, p.id, p.name, p.tenant_id \
                 FROM environments e JOIN projects p ON p.id = e.project_id \
                 WHERE e.id = ?1 AND e.token = ?2",
                    params![env.id().to_string(), env.token().to_string()],
                    |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, i64>(1)?,
                            r.get::<_, String>(2)?,
                            r.get::<_, String>(3)?,
                            r.get::<_, String>(4)?,
                        ))
                    },
                )
                .optional()?
                .ok_or_else(|| refused("no such environment"))?;
            let (name, created_at, project_id, project_name, tenant_id) = row;
            Ok(EnvironmentRecord {
                env: *env,
                name,
                project_id: project_id.parse().map_err(|_| refused("bad project ID"))?,
                project_name,
                tenant_id: tenant_id.parse().map_err(|_| refused("bad tenant ID"))?,
                created_at: Timestamp::from_millis(created_at),
            })
        })
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>, Error> {
        // Poisoned: a call panicked holding it.
        self.conn.lock().map_err(|_| refused("poisoned"))
    }
}

/// Where the bootstrap in a file with a schema stands.
fn read_state(conn: &Connection) -> Result<BootstrapState, Error> {
    let row = conn
        .query_row(
            "SELECT id, token, ledger_created_at IS NOT NULL FROM environments",
            [],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get(2)?)),
        )
        .optional()?;
    Ok(match row {
        None => BootstrapState::Unbootstrapped,
        Some((id, token, marked)) => {
            let env = env_ref(&id, &token)?;
            if marked {
                BootstrapState::Marked { env }
            } else {
                BootstrapState::Unmarked { env }
            }
        }
    })
}

/// The key `id`, if it was made for `env`'s ID and incarnation token.
fn lookup(conn: &Connection, id: KeyId, env: &EnvRef) -> Result<Option<KeyRecord>, Error> {
    let row = conn
        .query_row(
            "SELECT scopes, digest, expires_at, expiry_recorded, revoked_at FROM keys \
             WHERE id = ?1 AND environment_id = ?2 AND environment_token = ?3",
            params![
                id.to_string(),
                env.id().to_string(),
                env.token().to_string()
            ],
            |r| {
                Ok(Stored {
                    scopes: r.get(0)?,
                    digest: r.get(1)?,
                    expires_at: r.get(2)?,
                    expiry_recorded: r.get(3)?,
                    revoked_at: r.get(4)?,
                })
            },
        )
        .optional()?;
    row.map(|stored| stored.record(id, env)).transpose()
}

/// Opens `path` so that reading it leaves its bytes as they are, and no file beside it that was
/// not there but `-shm`, which SQLite derives from a WAL a crash left.
fn open_to_read(path: &Path) -> rusqlite::Result<Connection> {
    // A read-only connection leaves behind the WAL files it made; a writable one removes them,
    // but checkpoints a WAL it found.
    let mut wal = path.as_os_str().to_owned();
    wal.push("-wal");
    let flags = if Path::new(&wal).exists() {
        OpenFlags::SQLITE_OPEN_READ_ONLY
    } else {
        OpenFlags::SQLITE_OPEN_READ_WRITE
    };
    Connection::open_with_flags(path, flags)
}

/// What the key stored under `seed`'s key ID, if any, makes of `seed` in `env`. The outer error
/// is SQLite's, or that `env` is not this file's environment.
fn seed_state(
    conn: &Connection,
    env: &EnvRef,
    seed: &KeyParts,
) -> Result<Result<Option<SeedState>, SeedConflict>, Error> {
    let here: bool = conn.query_row(
        "SELECT count(*) > 0 FROM environments WHERE id = ?1 AND token = ?2",
        params![env.id().to_string(), env.token().to_string()],
        |r| r.get(0),
    )?;
    if !here {
        return Err(refused("no such environment"));
    }
    // Of any environment: a key ID is used once in the file.
    let origin = conn
        .query_row(
            "SELECT origin FROM keys WHERE id = ?1",
            [seed.id.to_string()],
            |r| r.get::<_, String>(0),
        )
        .optional()?;
    let Some(origin) = origin else {
        return Ok(Ok(None));
    };
    let origin = origin_from(&origin)?;
    Ok(lookup(conn, seed.id, env)?
        .ok_or(SeedConflict)
        .and_then(|record| verify_seed(seed, &record, origin, env))
        .map(Some))
}

/// Marks `env`'s ledger created, if it is this file's environment and unmarked.
fn mark(conn: &Connection, env: &EnvRef, now: Timestamp) -> Result<(), Error> {
    let changed = conn.execute(
        "UPDATE environments SET ledger_created_at = ?1 \
         WHERE id = ?2 AND token = ?3 AND ledger_created_at IS NULL",
        params![
            now.as_millis(),
            env.id().to_string(),
            env.token().to_string()
        ],
    )?;
    if changed == 1 {
        Ok(())
    } else {
        Err(refused("no such unmarked environment"))
    }
}

/// Revokes `env`'s unrevoked keys of `origin`; how many.
fn revoke_all(
    conn: &Connection,
    env: &EnvRef,
    origin: &str,
    now: Timestamp,
) -> Result<usize, Error> {
    Ok(conn.execute(
        "UPDATE keys SET revoked_at = ?1 WHERE environment_id = ?2 \
         AND environment_token = ?3 AND origin = ?4 AND revoked_at IS NULL",
        params![
            now.as_millis(),
            env.id().to_string(),
            env.token().to_string(),
            origin
        ],
    )?)
}

/// Inserts an admin key no key created: every scope, no expiry.
fn insert_admin(
    conn: &Connection,
    key: &KeyParts,
    env: &EnvRef,
    origin: &str,
    label: Option<&str>,
    now: Timestamp,
) -> Result<(), Error> {
    let every = BTreeSet::from(Scope::ALL);
    conn.execute(
        "INSERT INTO keys (id, environment_id, environment_token, scopes, digest, origin, \
         label, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            key.id.to_string(),
            env.id().to_string(),
            env.token().to_string(),
            scopes_text(&every),
            key.digest.to_stored(),
            origin,
            label,
            now.as_millis()
        ],
    )?;
    Ok(())
}

/// Sets `id`'s recorded expiry unless it is set or the key is revoked.
fn set_expiry_recorded(conn: &Connection, id: KeyId, env: &EnvRef) -> Result<(), Error> {
    conn.execute(
        "UPDATE keys SET expiry_recorded = 1 WHERE id = ?1 AND environment_id = ?2 \
         AND environment_token = ?3 AND revoked_at IS NULL AND expiry_recorded = 0",
        params![
            id.to_string(),
            env.id().to_string(),
            env.token().to_string()
        ],
    )?;
    Ok(())
}

/// Commits a transaction that recorded an expiry.
fn commit_expiry(tx: Transaction<'_>) -> rusqlite::Result<()> {
    #[cfg(feature = "__fault-points")]
    ironflowx_core::reach_fault_point(ironflowx_core::FaultPoint::BeforeExpiryCommit);
    tx.commit()?;
    #[cfg(feature = "__fault-points")]
    ironflowx_core::reach_fault_point(ironflowx_core::FaultPoint::AfterExpiryCommit);
    Ok(())
}

/// The calling key of a key change, read again inside its transaction and checked at the
/// transaction's now, with the transaction to go on with: `None` if the key is unknown, revoked
/// or expired by then (D43). A newly expired key's expiry is recorded, ending the transaction.
fn caller<'c>(
    tx: Transaction<'c>,
    id: KeyId,
    env: &EnvRef,
    now: Timestamp,
) -> Result<Option<(Transaction<'c>, Grant)>, Error> {
    let Some(record) = lookup(&tx, id, env)? else {
        return Ok(None);
    };
    match recheck(&record, env, now) {
        Ok(grant) => Ok(Some((tx, grant))),
        Err(refused) => {
            if refused.newly_expired() && set_expiry_recorded(&tx, id, env).is_ok() {
                // Refused alike whether or not the expiry was recorded (TEN-7).
                let _ = commit_expiry(tx);
            }
            Ok(None)
        }
    }
}

/// A key change's maker, as [`caller`] reads it: a key is read again and checked; an embedder,
/// which no record backs (D121), is `Some((tx, None))` while `env` is a current environment and
/// incarnation, as a key's record is checked to be.
fn changer<'c>(
    tx: Transaction<'c>,
    by: &Emitter,
    env: &EnvRef,
    now: Timestamp,
) -> Result<Option<(Transaction<'c>, Option<Grant>)>, Error> {
    Ok(match by.key_id() {
        Some(id) => caller(tx, id, env, now)?.map(|(tx, grant)| (tx, Some(grant))),
        None => {
            let here: bool = tx.query_row(
                "SELECT count(*) > 0 FROM environments WHERE id = ?1 AND token = ?2",
                params![env.id().to_string(), env.token().to_string()],
                |r| r.get(0),
            )?;
            here.then_some((tx, None))
        }
    })
}

/// A creator as stored: a creating key's bare ID, as since 0.2, or an embedder's
/// `embedder:<name>`.
fn stored_creator(by: &Emitter) -> String {
    by.key_id()
        .map_or_else(|| by.to_string(), |id| id.to_string())
}

/// A stored creator, as [`stored_creator`] writes it.
fn creator_from(text: &str) -> Result<Emitter, Error> {
    match text.parse::<KeyId>() {
        Ok(id) => Ok(id.into()),
        Err(_) if text.starts_with("embedder:") => text.parse().map_err(|_| refused("bad creator")),
        Err(_) => Err(refused("bad creator ID")),
    }
}

/// A key row's credential columns, as read.
struct Stored {
    scopes: String,
    digest: Vec<u8>,
    expires_at: Option<i64>,
    expiry_recorded: bool,
    revoked_at: Option<i64>,
}

impl Stored {
    fn record(self, id: KeyId, env: &EnvRef) -> Result<KeyRecord, Error> {
        Ok(KeyRecord {
            id,
            env: *env,
            scopes: parse_scopes(&self.scopes)?,
            digest: KeyDigest::from_stored(&self.digest).ok_or_else(|| refused("bad digest"))?,
            expires_at: self.expires_at.map(Timestamp::from_millis),
            expiry_recorded: self.expiry_recorded,
            revoked_at: self.revoked_at.map(Timestamp::from_millis),
        })
    }
}

fn origin_from(text: &str) -> Result<KeyOrigin, Error> {
    match text {
        MINTED => Ok(KeyOrigin::Minted),
        SEED => Ok(KeyOrigin::Seed),
        RECOVERED => Ok(KeyOrigin::Recovered),
        CREATED => Ok(KeyOrigin::Created),
        _ => Err(refused("bad key origin")),
    }
}

/// Whether the file holds this format's schema; false only for a file with nothing in it. One
/// without `meta`, no control plane, is an error.
fn has_schema(conn: &Connection) -> Result<bool, Error> {
    let empty: bool = conn.query_row("SELECT count(*) = 0 FROM sqlite_master", [], |r| r.get(0))?;
    if empty {
        return Ok(false);
    }
    let format: i64 = conn.query_row("SELECT format FROM meta", [], |r| r.get(0))?;
    if format == i64::from(FORMAT) {
        Ok(true)
    } else {
        Err(Error::Format)
    }
}

fn env_ref(id: &str, token: &str) -> Result<EnvRef, Error> {
    format!("{id}.{token}")
        .parse()
        .map_err(|_| refused("bad environment reference"))
}

/// Scopes as text: their names, sorted, comma-separated.
fn scopes_text(scopes: &BTreeSet<Scope>) -> String {
    scopes
        .iter()
        .map(Scope::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

fn parse_scopes(text: &str) -> Result<BTreeSet<Scope>, Error> {
    text.split(',')
        .map(|s| s.parse().map_err(|_| refused("bad scope")))
        .collect()
}
