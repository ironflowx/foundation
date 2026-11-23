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

use ironflowx_authz::{CreateRefusal, Grant, KeyDigest, KeyParts, KeyRecord, may_create, recheck};
use ironflowx_core::{Clock, EnvRef, KeyId, KeyOrigin, ProjectId, Scope, TenantId, Timestamp};
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
    /// The key that created it; `None` unless its origin is `Created`.
    pub created_by: Option<KeyId>,
    pub label: Option<String>,
    pub created_at: Timestamp,
}

/// Why a key change did not happen. Nothing was written, but for a calling key found newly
/// expired: its expiry is recorded (D42).
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
    /// If SQLite fails; nothing is recorded then.
    pub fn record_expiry(&self, env: &EnvRef, id: KeyId) -> Result<(), StoreError> {
        run(|| {
            let mut conn = self.lock()?;
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            set_expiry_recorded(&tx, id, env)?;
            tx.commit()?;
            Ok(())
        })
    }

    /// Creates `new` in `env` for `creator`, in one transaction: with the write lock held and
    /// now read from `clock`, the creator's record is read again and checked (`recheck`), then
    /// the request (`may_create`), so no key outlives or outranks its creator (TEN-5), then
    /// that fewer than `cap` created keys hold a slot: one does until it is revoked or its
    /// expiry is recorded, whatever the clock reads (TEN-15).
    ///
    /// # Errors
    ///
    /// `Caller`, `Refused` or `Full` as the checks find, `Store` if SQLite fails; no key is
    /// created. A newly expired `creator` has its expiry recorded.
    pub fn insert_key(
        &self,
        env: &EnvRef,
        new: NewKey,
        creator: KeyId,
        clock: &dyn Clock,
        cap: u32,
    ) -> Result<KeyRow, KeyChangeError> {
        // The outer error is SQLite's; the inner one, a check that refused.
        run(|| {
            let mut conn = self.lock()?;
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let now = clock.now();
            let Some((tx, grant)) = caller(tx, creator, env, now)? else {
                return Ok(Err(KeyChangeError::Caller));
            };
            let label = new.label.as_deref();
            if let Err(r) = may_create(&grant, &new.scopes, new.expires_at, label, now) {
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
                    creator.to_string(),
                    label,
                    new.expires_at.map(Timestamp::as_millis),
                    now.as_millis()
                ],
            )?;
            let seq = u64::try_from(tx.last_insert_rowid()).map_err(|_| refused("bad seq"))?;
            tx.commit()?;
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
                created_by: Some(creator),
                label: new.label,
                created_at: now,
            }))
        })
        .map_err(KeyChangeError::Store)?
    }

    /// Revokes `id` in `env` for `by`, in one transaction: with the write lock held and now read
    /// from `clock`, `by`'s record is read again and checked (`recheck`, D43), then `id`'s
    /// `revoked_at` is set unless it already is. The row stays (D35).
    ///
    /// # Errors
    ///
    /// `Caller` if `by` is no longer valid, `NotFound` if `env` holds no key `id`, `Store` if
    /// SQLite fails; no key is revoked. A newly expired `by` has its expiry recorded.
    pub fn revoke_key(
        &self,
        env: &EnvRef,
        id: KeyId,
        by: KeyId,
        clock: &dyn Clock,
    ) -> Result<(), KeyChangeError> {
        // The outer error is SQLite's; the inner one, a check that refused.
        run(|| {
            let mut conn = self.lock()?;
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let now = clock.now();
            let Some((tx, _)) = caller(tx, by, env, now)? else {
                return Ok(Err(KeyChangeError::Caller));
            };
            if lookup(&tx, id, env)?.is_none() {
                return Ok(Err(KeyChangeError::NotFound));
            }
            tx.execute(
                "UPDATE keys SET revoked_at = ?1 WHERE id = ?2 AND revoked_at IS NULL",
                params![now.as_millis(), id.to_string()],
            )?;
            tx.commit()?;
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
                    created_by: created_by
                        .map(|c| c.parse().map_err(|_| refused("bad creator ID")))
                        .transpose()?,
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
                let _ = tx.commit();
            }
            Ok(None)
        }
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
