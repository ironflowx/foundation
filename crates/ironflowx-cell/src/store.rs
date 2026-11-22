//! The read store: `read.db`, derived from the log and never trusted over it (D14).

use std::fmt;
use std::path::Path;

use ironflowx_core::{EnvRef, EventId, Timestamp};
use ironflowx_events::{Event, EventTables, EventsView, Record};
use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSql, ToSqlOutput, ValueRef};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::record::FORMAT;

/// The read store failed.
#[derive(Debug)]
pub struct StoreError(rusqlite::Error);

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "read store: {}", self.0)
    }
}

impl std::error::Error for StoreError {}

/// Private, so that `rusqlite` stays out of the public API.
fn store_err<T>(r: rusqlite::Result<T>) -> Result<T, StoreError> {
    r.map_err(StoreError)
}

/// The read store as event rules' queries see it.
pub struct ReadStore<'a>(pub(crate) &'a Connection);

/// D18: the SQLite conversions over `Timestamp`'s one `i64` encoding.
struct Millis(Timestamp);

impl ToSql for Millis {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(self.0.as_millis().into())
    }
}

impl FromSql for Millis {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        i64::column_result(value).map(|m| Self(Timestamp::from_millis(m)))
    }
}

/// An ID stored as its text form.
struct Id(EventId);

impl FromSql for Id {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        value
            .as_str()?
            .parse()
            .map(Self)
            .map_err(|e| FromSqlError::Other(Box::new(e)))
    }
}

const SCHEMA: &str = "
CREATE TABLE events (
    sequence INTEGER PRIMARY KEY,
    id TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    data BLOB NOT NULL,
    metadata BLOB,
    version INTEGER NOT NULL,
    idempotency_key TEXT,
    time INTEGER NOT NULL
);
CREATE TABLE idempotency (key TEXT PRIMARY KEY, event_id TEXT NOT NULL);
CREATE TABLE meta (applied INTEGER NOT NULL, env_id TEXT NOT NULL, token TEXT NOT NULL, format INTEGER NOT NULL);
";

/// Opens `read.db` if it exists, has `meta`, agrees with the genesis record and is not ahead of
/// the log. Otherwise, or with `rebuild`, deletes it and creates an empty one at applied
/// position 0.
pub(crate) fn open(
    dir: &Path,
    env: EnvRef,
    watermark: u64,
    rebuild: bool,
) -> std::io::Result<Connection> {
    let path = dir.join("read.db");
    if !rebuild && let Some(conn) = existing(&path, env, watermark) {
        return Ok(conn);
    }
    for suffix in ["", "-wal", "-shm"] {
        let mut p = path.clone().into_os_string();
        p.push(suffix);
        match std::fs::remove_file(&p) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
    }
    create(&path, env).map_err(std::io::Error::other)
}

fn create(path: &Path, env: EnvRef) -> rusqlite::Result<Connection> {
    let conn = Connection::open(path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.execute_batch(SCHEMA)?;
    conn.execute(
        "INSERT INTO meta VALUES (0, ?1, ?2, ?3)",
        params![env.id().to_string(), env.token().to_string(), FORMAT],
    )?;
    Ok(conn)
}

/// The applied position `read.db` records, if it is readable and its `meta` names this
/// environment and format. With an empty log, any recorded progress is evidence of loss.
pub(crate) fn recorded(dir: &Path, env: EnvRef, empty_log: bool) -> std::io::Result<Option<u64>> {
    let path = dir.join("read.db");
    if !path.try_exists()? {
        return Ok(None);
    }
    if empty_log {
        // Without a genesis there is no authoritative log to rebuild from. Never
        // interpret a failed metadata read as permission to replace this file.
        let conn = Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(std::io::Error::other)?;
        return applied(&conn).map(Some).map_err(std::io::Error::other);
    }
    Ok(Connection::open(&path)
        .ok()
        .and_then(|conn| ours(&conn, env)))
}

fn ours(conn: &Connection, env: EnvRef) -> Option<u64> {
    let (applied, id, token, format): (u64, String, String, u16) = conn
        .query_row("SELECT applied, env_id, token, format FROM meta", [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })
        .ok()?;
    let ours = id == env.id().to_string() && token == env.token().to_string() && format == FORMAT;
    ours.then_some(applied)
}

fn existing(path: &Path, env: EnvRef, watermark: u64) -> Option<Connection> {
    if !path.exists() {
        return None;
    }
    let conn = Connection::open(path).ok()?;
    let agrees = ours(&conn, env).is_some_and(|applied| applied <= watermark);
    // `meta` can agree while the tables are damaged, and replay may touch none of them.
    let intact = || -> rusqlite::Result<bool> {
        conn.prepare(&format!("SELECT {EVENT_COLUMNS} LIMIT 0"))?;
        conn.prepare("SELECT key, event_id FROM idempotency LIMIT 0")?;
        let check: String = conn.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
        Ok(check == "ok")
    };
    (agrees && intact().unwrap_or(false)).then_some(conn)
}

pub(crate) fn last_event(conn: &Connection) -> rusqlite::Result<Option<(u64, EventId)>> {
    conn.query_row(
        "SELECT sequence, id FROM events ORDER BY sequence DESC LIMIT 1",
        [],
        |r| Ok((r.get(0)?, r.get::<_, Id>(1)?.0)),
    )
    .optional()
}

pub(crate) fn applied(conn: &Connection) -> rusqlite::Result<u64> {
    conn.query_row("SELECT applied FROM meta", [], |r| r.get(0))
}

/// Folds the record at `position` and advances the applied position past it, in one
/// transaction. A record below the applied position is skipped (LED-12). The genesis record
/// has no events: `None`. `recovering` when open folds it, for the crash test's fault points.
pub(crate) fn apply(
    conn: &mut Connection,
    position: u64,
    record: Option<&Record>,
    #[cfg_attr(not(feature = "__fault-points"), allow(unused_variables))] recovering: bool,
) -> rusqlite::Result<()> {
    let tx = conn.transaction()?;
    if position < applied(&tx)? {
        return Ok(());
    }
    if let Some(record) = record {
        ironflowx_events::fold(&mut Tables(&tx), record)?;
        #[cfg(feature = "__fault-points")]
        ironflowx_core::reach_fault_point(if recovering {
            ironflowx_core::FaultPoint::DuringOpen
        } else {
            ironflowx_core::FaultPoint::MidFold
        });
    }
    tx.execute("UPDATE meta SET applied = ?1", [position + 1])?;
    tx.commit()
}

struct Tables<'a>(&'a Transaction<'a>);

impl EventTables for Tables<'_> {
    type Error = rusqlite::Error;

    fn insert(&mut self, e: Event) -> rusqlite::Result<()> {
        let id = e.id.to_string();
        self.0.execute(
            "INSERT INTO events VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                e.sequence,
                id,
                e.name,
                e.data,
                e.metadata,
                e.version,
                e.idempotency_key,
                Millis(e.time)
            ],
        )?;
        if let Some(key) = &e.idempotency_key {
            self.0
                .execute("INSERT INTO idempotency VALUES (?1, ?2)", params![key, id])?;
        }
        Ok(())
    }
}

const EVENT_COLUMNS: &str =
    "sequence, id, name, data, metadata, version, idempotency_key, time FROM events";

fn row_event(r: &rusqlite::Row<'_>) -> rusqlite::Result<Event> {
    Ok(Event {
        sequence: r.get(0)?,
        id: r.get::<_, Id>(1)?.0,
        name: r.get(2)?,
        data: r.get(3)?,
        metadata: r.get(4)?,
        version: r.get(5)?,
        idempotency_key: r.get(6)?,
        time: r.get::<_, Millis>(7)?.0,
    })
}

impl EventsView for ReadStore<'_> {
    type Error = StoreError;

    fn event_by_key(&self, key: &str) -> Result<Option<EventId>, StoreError> {
        let sql = "SELECT event_id FROM idempotency WHERE key = ?1";
        let id = self
            .0
            .query_row(sql, [key], |r| r.get::<_, Id>(0))
            .optional();
        store_err(id).map(|id| id.map(|id| id.0))
    }

    fn last_sequence(&self) -> Result<u64, StoreError> {
        let sql = "SELECT COALESCE(MAX(sequence), 0) FROM events";
        store_err(self.0.query_row(sql, [], |r| r.get(0)))
    }

    fn event(&self, id: EventId) -> Result<Option<Event>, StoreError> {
        let sql = format!("SELECT {EVENT_COLUMNS} WHERE id = ?1");
        store_err(
            self.0
                .query_row(&sql, [id.to_string()], row_event)
                .optional(),
        )
    }

    fn events_before(&self, before: Option<u64>, limit: usize) -> Result<Vec<Event>, StoreError> {
        let sql =
            format!("SELECT {EVENT_COLUMNS} WHERE sequence < ?1 ORDER BY sequence DESC LIMIT ?2");
        // A cursor past every sequence lists from the newest.
        let before = before.map_or(i64::MAX, |b| i64::try_from(b).unwrap_or(i64::MAX));
        store_err(self.0.prepare(&sql).and_then(|mut stmt| {
            stmt.query_map(
                params![before, i64::try_from(limit).unwrap_or(i64::MAX)],
                row_event,
            )?
            .collect()
        }))
    }
}
