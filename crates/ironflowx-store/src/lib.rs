//! The Ironflow X control plane: which tenants, projects, environments and keys exist, in
//! `<root>/control.db` (SQLite).
//!
//! Blocking: the engine calls it under `spawn_blocking`. Every change is one transaction that
//! takes SQLite's write lock first, then reads now from the clock it is given (TEN-11).

#![forbid(unsafe_code)]

use std::collections::BTreeSet;
use std::fmt;
use std::path::Path;
use std::sync::Mutex;

use ironflowx_authz::{KeyDigest, KeyParts, KeyRecord};
use ironflowx_core::{Clock, EnvRef, KeyId, ProjectId, Scope, TenantId, Timestamp};
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};

/// The control plane's format. A change refuses older files at open.
const FORMAT: u16 = 1;
/// What bootstrap names the default tenant, project and environment.
const DEFAULT: &str = "default";
const MINTED: &str = "minted";

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
    created_at INTEGER NOT NULL
);
";

/// A control-plane read or write failed.
#[derive(Debug)]
pub struct StoreError(String);

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "control plane: {}", self.0)
    }
}

impl std::error::Error for StoreError {}

/// The private failure, so that `rusqlite` stays out of the public API.
enum Error {
    Db(rusqlite::Error),
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
        Error::Db(e) => StoreError(e.to_string()),
        Error::Refused(why) => StoreError(why.to_owned()),
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

/// What one bootstrap creates: the default tenant, project and environment, and its admin key,
/// origin minted. The engine draws every ID and the key's secret.
#[derive(Debug, Clone)]
pub struct Bootstrap {
    pub env: EnvRef,
    pub tenant: TenantId,
    pub project: ProjectId,
    pub key: KeyParts,
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
    /// If the file is in another format, or SQLite fails.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        run(|| {
            let mut conn = Connection::open(path)?;
            conn.pragma_update(None, "journal_mode", "WAL")?;
            conn.pragma_update(None, "synchronous", "FULL")?;
            // A no-op except on macOS, where fsync alone does not reach the disk.
            conn.pragma_update(None, "fullfsync", true)?;
            conn.pragma_update(None, "foreign_keys", true)?;
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let has_meta: bool = tx.query_row(
                "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'meta'",
                [],
                |r| r.get(0),
            )?;
            if has_meta {
                check_format(&tx)?;
            } else {
                tx.execute_batch(SCHEMA)?;
                tx.execute("INSERT INTO meta VALUES (?1)", [FORMAT])?;
            }
            tx.commit()?;
            Ok(Self {
                conn: Mutex::new(conn),
            })
        })
    }

    /// Reads where the bootstrap at `path` stands. Opens the file read-only and creates nothing.
    ///
    /// # Errors
    ///
    /// If the file is in another format, or SQLite fails.
    pub fn bootstrap_state(path: &Path) -> Result<BootstrapState, StoreError> {
        run(|| {
            if !path
                .try_exists()
                .map_err(|_| refused("cannot read the data directory"))?
            {
                return Ok(BootstrapState::NoFile);
            }
            let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
            check_format(&conn)?;
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
        })
    }

    /// Creates the default tenant, project and environment and the minted admin key, in one
    /// transaction.
    ///
    /// # Errors
    ///
    /// If the file is already bootstrapped, or SQLite fails; nothing is written then.
    pub fn ensure_bootstrap(&self, b: &Bootstrap, clock: &dyn Clock) -> Result<(), StoreError> {
        run(|| {
            let mut conn = self.lock()?;
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let now = clock.now().as_millis();
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
            let admin = BTreeSet::from([Scope::Read, Scope::Write, Scope::Admin]);
            tx.execute(
                "INSERT INTO keys (id, environment_id, environment_token, scopes, digest, origin, \
             created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    b.key.id.to_string(),
                    env.id().to_string(),
                    env.token().to_string(),
                    scopes_text(&admin),
                    b.key.digest.to_stored(),
                    MINTED,
                    now
                ],
            )?;
            tx.commit()?;
            Ok(())
        })
    }

    /// Marks `env`'s ledger created: the bootstrap is finished.
    ///
    /// # Errors
    ///
    /// If `env` is not this file's environment, or SQLite fails.
    pub fn mark_ledger_created(&self, env: &EnvRef, clock: &dyn Clock) -> Result<(), StoreError> {
        run(|| {
            let mut conn = self.lock()?;
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let now = clock.now().as_millis();
            let changed = tx.execute(
                "UPDATE environments SET ledger_created_at = ?1 WHERE id = ?2 AND token = ?3",
                params![now, env.id().to_string(), env.token().to_string()],
            )?;
            if changed != 1 {
                return Err(refused("no such environment"));
            }
            tx.commit()?;
            Ok(())
        })
    }

    /// The key `id`, if it was made for `env`'s ID and incarnation token. Admission's one read.
    ///
    /// # Errors
    ///
    /// If SQLite fails, or the stored row does not decode.
    pub fn lookup_key(&self, id: KeyId, env: &EnvRef) -> Result<Option<KeyRecord>, StoreError> {
        run(|| {
            let conn = self.lock()?;
            let row = conn
                .query_row(
                    "SELECT scopes, digest FROM keys \
                 WHERE id = ?1 AND environment_id = ?2 AND environment_token = ?3",
                    params![
                        id.to_string(),
                        env.id().to_string(),
                        env.token().to_string()
                    ],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?)),
                )
                .optional()?;
            row.map(|(scopes, digest)| {
                Ok(KeyRecord {
                    id,
                    env: *env,
                    scopes: parse_scopes(&scopes)?,
                    digest: KeyDigest::from_stored(&digest).ok_or_else(|| refused("bad digest"))?,
                })
            })
            .transpose()
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

fn check_format(conn: &Connection) -> Result<(), Error> {
    let format: u16 = conn.query_row("SELECT format FROM meta", [], |r| r.get(0))?;
    if format == FORMAT {
        Ok(())
    } else {
        Err(refused("another format version"))
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
