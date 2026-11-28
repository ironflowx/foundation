//! The read store: `read.db`, derived from the log and never trusted over it (D14).

use std::fmt;
use std::path::Path;

use ironflowx_core::{Emitter, EnvRef, EventId, RunId, Timestamp};
use ironflowx_events::{Event, EventTables, EventsView, PageBudget};
use ironflowx_execution::{
    Definition, ExecutionTables, ExecutionView, Function, Run, RunError, Status, Step,
};
use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSql, ToSqlOutput, ValueRef};
use rusqlite::{Connection, OptionalExtension, Transaction, params, params_from_iter};
use uuid::Uuid;

use crate::record::{Entries, FORMAT};

/// The read store failed.
#[derive(Debug)]
pub(crate) struct StoreError(rusqlite::Error);

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
pub(crate) struct ReadStore<'a>(pub(crate) &'a Connection);

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

/// A value stored as its text form: an event ID or an emitter.
struct Text<T>(T);

impl<T: std::str::FromStr<Err = ironflowx_core::ParseError>> FromSql for Text<T> {
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
    time INTEGER NOT NULL,
    emitted_by TEXT NOT NULL
);
CREATE TABLE idempotency (key TEXT PRIMARY KEY, event_id TEXT NOT NULL);
CREATE TABLE meta (applied INTEGER NOT NULL, env_id TEXT NOT NULL, token TEXT NOT NULL, format INTEGER NOT NULL, record_id TEXT);
CREATE TABLE functions (
    name TEXT PRIMARY KEY,
    code TEXT NOT NULL,
    max_attempts INTEGER,
    first_delay_seconds INTEGER,
    max_delay_seconds INTEGER,
    put_at INTEGER NOT NULL,
    put_by TEXT NOT NULL
);
CREATE TABLE triggers (
    function TEXT NOT NULL,
    event_name TEXT NOT NULL,
    PRIMARY KEY (function, event_name)
);
CREATE TABLE runs (
    id TEXT PRIMARY KEY,
    sequence INTEGER NOT NULL UNIQUE,
    function TEXT NOT NULL,
    code TEXT NOT NULL,
    status TEXT NOT NULL,
    input BLOB NOT NULL,
    created_at INTEGER NOT NULL,
    created_by TEXT NOT NULL,
    due_at INTEGER,
    execution INTEGER NOT NULL,
    lease_until INTEGER,
    lease_seconds INTEGER NOT NULL,
    claimed_by TEXT,
    lost_leases INTEGER NOT NULL,
    steps INTEGER NOT NULL,
    step TEXT,
    output BLOB,
    error_code TEXT,
    error_message TEXT,
    ended_at INTEGER
);
-- The claim's query, one branch each: a pair queued by due time, and running by lease deadline.
CREATE INDEX runs_due ON runs (function, code, status, due_at, sequence);
CREATE INDEX runs_lapse ON runs (function, code, status, lease_until, sequence);
CREATE TABLE steps (
    run_id TEXT NOT NULL,
    position INTEGER NOT NULL,
    name TEXT NOT NULL,
    once INTEGER NOT NULL,
    input BLOB,
    execution INTEGER NOT NULL,
    output BLOB,
    PRIMARY KEY (run_id, position)
);
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
        "INSERT INTO meta VALUES (0, ?1, ?2, ?3, NULL)",
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
        conn.prepare(&format!("SELECT {FUNCTION_COLUMNS} LIMIT 0"))?;
        conn.prepare("SELECT function, event_name FROM triggers LIMIT 0")?;
        conn.prepare(&format!("SELECT {RUN_COLUMNS} LIMIT 0"))?;
        // `run_id` too, which the step lookup reads beside the columns it returns.
        conn.prepare(&format!("SELECT run_id, {STEP_COLUMNS} LIMIT 0"))?;
        let check: String = conn.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
        Ok(check == "ok")
    };
    (agrees && intact().unwrap_or(false)).then_some(conn)
}

/// The ID of the last applied record, as stored; none while only the genesis record is (D117).
pub(crate) fn last_record(conn: &Connection) -> rusqlite::Result<Option<String>> {
    conn.query_row("SELECT record_id FROM meta", [], |r| r.get(0))
}

pub(crate) fn applied(conn: &Connection) -> rusqlite::Result<u64> {
    conn.query_row("SELECT applied FROM meta", [], |r| r.get(0))
}

/// Folds the record at `position` and advances the applied position past it, with the record's
/// ID, in one transaction. A record below the applied position is skipped (LED-12). The genesis
/// record has no ID and no entries: `None`; any other comes with its ID and emitter. `recovering`
/// when open folds it, for the crash test's fault points.
pub(crate) fn apply(
    conn: &mut Connection,
    position: u64,
    record: Option<(Uuid, &Entries, &Emitter)>,
    #[cfg_attr(not(feature = "__fault-points"), allow(unused_variables))] recovering: bool,
) -> rusqlite::Result<()> {
    let tx = conn.transaction()?;
    if position < applied(&tx)? {
        return Ok(());
    }
    if let Some((_, entries, emitted_by)) = record {
        ironflowx_events::fold(&mut Tables(&tx), &entries.record, emitted_by)?;
        for entry in &entries.execution {
            ironflowx_execution::fold(&mut Tables(&tx), entry, entries.record.time, emitted_by)?;
        }
        #[cfg(feature = "__fault-points")]
        ironflowx_core::reach_fault_point(if recovering {
            ironflowx_core::FaultPoint::DuringOpen
        } else {
            ironflowx_core::FaultPoint::MidFold
        });
    }
    tx.execute(
        "UPDATE meta SET applied = ?1, record_id = ?2",
        params![position + 1, record.map(|(id, ..)| id.to_string())],
    )?;
    tx.commit()
}

struct Tables<'a>(&'a Transaction<'a>);

impl EventTables for Tables<'_> {
    type Error = rusqlite::Error;

    fn insert(&mut self, e: Event) -> rusqlite::Result<()> {
        let id = e.id.to_string();
        self.0.execute(
            "INSERT INTO events VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                e.sequence,
                id,
                e.name,
                e.data,
                e.metadata,
                e.version,
                e.idempotency_key,
                Millis(e.time),
                e.emitted_by.to_string()
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
    "sequence, id, name, data, metadata, version, idempotency_key, time, emitted_by FROM events";

fn row_event(r: &rusqlite::Row<'_>) -> rusqlite::Result<Event> {
    Ok(Event {
        sequence: r.get(0)?,
        id: r.get::<_, Text<EventId>>(1)?.0,
        name: r.get(2)?,
        data: r.get(3)?,
        metadata: r.get(4)?,
        version: r.get(5)?,
        idempotency_key: r.get(6)?,
        time: r.get::<_, Millis>(7)?.0,
        emitted_by: r.get::<_, Text<Emitter>>(8)?.0,
    })
}

impl EventsView for ReadStore<'_> {
    type Error = StoreError;

    fn event_by_key(&self, key: &str) -> Result<Option<EventId>, StoreError> {
        let sql = "SELECT event_id FROM idempotency WHERE key = ?1";
        let id = self
            .0
            .query_row(sql, [key], |r| r.get::<_, Text<EventId>>(0))
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

    fn events_before(
        &self,
        before: Option<u64>,
        budget: &mut PageBudget,
    ) -> Result<Vec<Event>, StoreError> {
        // A cursor past every sequence lists from the newest.
        let before = before.map_or(i64::MAX, |b| i64::try_from(b).unwrap_or(i64::MAX));
        store_err((|| {
            // SQLite takes a blob's length from its row's header, so of the event that ends the
            // page it reads none of the overflow pages.
            let mut sizes = self.0.prepare(
                "SELECT sequence, length(data) + ifnull(length(metadata), 0) FROM events
                 WHERE sequence < ?1 ORDER BY sequence DESC",
            )?;
            let sql = format!("SELECT {EVENT_COLUMNS} WHERE sequence = ?1");
            let mut event = self.0.prepare(&sql)?;
            let mut events = Vec::new();
            // Both statements read one snapshot, the open query's.
            let mut rows = sizes.query([before])?;
            while let Some(row) = rows.next()? {
                if !budget.take(row.get(1)?) {
                    break;
                }
                events.push(event.query_row([row.get::<_, u64>(0)?], row_event)?);
            }
            Ok(events)
        })())
    }
}

/// One row changed, or the fold of an entry met a state it does not follow from.
fn one(changed: usize) -> rusqlite::Result<()> {
    match changed {
        1 => Ok(()),
        _ => Err(rusqlite::Error::QueryReturnedNoRows),
    }
}

impl ExecutionTables for Tables<'_> {
    type Error = rusqlite::Error;

    fn put_function(&mut self, f: &Function) -> rusqlite::Result<()> {
        let d = &f.definition;
        self.0.execute(
            "INSERT OR REPLACE INTO functions VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                d.name,
                d.code,
                d.max_attempts,
                d.first_delay_seconds,
                d.max_delay_seconds,
                Millis(f.put_at),
                f.put_by.to_string()
            ],
        )?;
        self.0
            .execute("DELETE FROM triggers WHERE function = ?1", [&d.name])?;
        for trigger in &d.triggers {
            self.0.execute(
                "INSERT INTO triggers VALUES (?1, ?2)",
                params![d.name, trigger],
            )?;
        }
        Ok(())
    }

    fn create_run(&mut self, r: &Run) -> rusqlite::Result<()> {
        self.0.execute(
            &format!("INSERT INTO runs ({RUN_FIELDS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)"),
            params![
                r.id.to_string(),
                r.sequence,
                r.function,
                r.code,
                r.status.as_str(),
                r.input,
                Millis(r.created_at),
                r.created_by.to_string(),
                r.due_at.map(Millis),
                r.execution,
                r.lease_until.map(Millis),
                r.lease_seconds,
                r.claimed_by.as_ref().map(ToString::to_string),
                r.lost_leases,
                r.steps,
                r.step,
                r.output,
                r.error.as_ref().map(|e| &e.code),
                r.error.as_ref().map(|e| &e.message),
                r.ended_at.map(Millis)
            ],
        )?;
        Ok(())
    }

    fn claim(
        &mut self,
        run: RunId,
        execution: u32,
        lease_until: Timestamp,
        lease_seconds: u32,
        by: &Emitter,
    ) -> rusqlite::Result<()> {
        one(self.0.execute(
            "UPDATE runs SET status = 'running', execution = ?2, lease_until = ?3,
             lease_seconds = ?4, claimed_by = ?5, due_at = NULL WHERE id = ?1",
            params![
                run.to_string(),
                execution,
                Millis(lease_until),
                lease_seconds,
                by.to_string()
            ],
        )?)
    }

    fn renew(&mut self, run: RunId, lease_until: Timestamp) -> rusqlite::Result<()> {
        one(self.0.execute(
            "UPDATE runs SET lease_until = ?2 WHERE id = ?1 AND status = 'running'",
            params![run.to_string(), Millis(lease_until)],
        )?)
    }

    fn lose_lease(&mut self, run: RunId, lost_leases: u32) -> rusqlite::Result<()> {
        one(self.0.execute(
            "UPDATE runs SET lost_leases = ?2 WHERE id = ?1 AND status = 'running'",
            params![run.to_string(), lost_leases],
        )?)
    }

    fn release(&mut self, run: RunId, at: Timestamp) -> rusqlite::Result<()> {
        one(self.0.execute(
            "UPDATE runs SET status = 'queued', due_at = ?2, lease_until = NULL
             WHERE id = ?1 AND status = 'running'",
            params![run.to_string(), Millis(at)],
        )?)
    }

    fn begin_step(&mut self, run: RunId, s: &Step) -> rusqlite::Result<()> {
        let id = run.to_string();
        self.0.execute(
            "INSERT INTO steps VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                id,
                s.position,
                s.name,
                s.once,
                s.input,
                s.execution,
                s.output
            ],
        )?;
        one(self.0.execute(
            "UPDATE runs SET steps = ?2, step = ?3 WHERE id = ?1",
            params![id, s.position, s.name],
        )?)
    }

    fn complete_step(&mut self, run: RunId, position: u32, output: &[u8]) -> rusqlite::Result<()> {
        let id = run.to_string();
        one(self.0.execute(
            "UPDATE steps SET output = ?3 WHERE run_id = ?1 AND position = ?2 AND output IS NULL",
            params![id, position, output],
        )?)?;
        one(self.0.execute(
            "UPDATE runs SET step = NULL, lost_leases = 0 WHERE id = ?1",
            [id],
        )?)
    }

    fn complete_run(&mut self, run: RunId, output: &[u8], at: Timestamp) -> rusqlite::Result<()> {
        one(self.0.execute(
            "UPDATE runs SET status = 'completed', output = ?2, ended_at = ?3, lease_until = NULL
             WHERE id = ?1",
            params![run.to_string(), output, Millis(at)],
        )?)
    }

    fn fail_run(&mut self, run: RunId, error: &RunError, at: Timestamp) -> rusqlite::Result<()> {
        one(self.0.execute(
            "UPDATE runs SET status = 'failed', error_code = ?2, error_message = ?3, ended_at = ?4,
             lease_until = NULL WHERE id = ?1 AND status = 'running'",
            params![run.to_string(), error.code, error.message, Millis(at)],
        )?)
    }
}

/// The claim's query of `pairs` pairs: ?1 is the time, ?2 the limit, and the pairs take ?3 on.
/// One branch per index, so neither reads ended runs: a queued run is due at its due time, a
/// running one at its deadline. `due` follows the columns `row_run` reads.
pub(crate) fn claim_sql(pairs: usize) -> String {
    let pair = |i: usize| format!("(function = ?{} AND code = ?{})", 2 * i + 3, 2 * i + 4);
    let pairs: Vec<_> = (0..pairs).map(pair).collect();
    let pairs = pairs.join(" OR ");
    format!(
        "SELECT {RUN_FIELDS}, due_at AS due FROM runs
         WHERE status = 'queued' AND due_at <= ?1 AND ({pairs})
         UNION ALL
         SELECT {RUN_FIELDS}, lease_until AS due FROM runs
         WHERE status = 'running' AND lease_until <= ?1 AND ({pairs})
         ORDER BY due, sequence LIMIT ?2"
    )
}

const FUNCTION_COLUMNS: &str = "name, code, max_attempts, first_delay_seconds, \
    max_delay_seconds, put_at, put_by FROM functions";

const RUN_FIELDS: &str = "id, sequence, function, code, status, input, created_at, created_by, \
    due_at, execution, lease_until, lease_seconds, claimed_by, lost_leases, steps, step, output, \
    error_code, error_message, ended_at";

const RUN_COLUMNS: &str = "id, sequence, function, code, status, input, created_at, created_by, \
    due_at, execution, lease_until, lease_seconds, claimed_by, lost_leases, steps, step, output, \
    error_code, error_message, ended_at FROM runs";

const STEP_COLUMNS: &str = "position, name, once, input, execution, output FROM steps";

/// A status as stored, which only `Status::as_str` writes.
struct StoredStatus(Status);

impl FromSql for StoredStatus {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        Status::from_name(value.as_str()?)
            .map(Self)
            .ok_or(FromSqlError::InvalidType)
    }
}

fn row_run(r: &rusqlite::Row<'_>) -> rusqlite::Result<Run> {
    Ok(Run {
        id: r.get::<_, Text<RunId>>(0)?.0,
        sequence: r.get(1)?,
        function: r.get(2)?,
        code: r.get(3)?,
        status: r.get::<_, StoredStatus>(4)?.0,
        input: r.get(5)?,
        created_at: r.get::<_, Millis>(6)?.0,
        created_by: r.get::<_, Text<Emitter>>(7)?.0,
        due_at: r.get::<_, Option<Millis>>(8)?.map(|m| m.0),
        execution: r.get(9)?,
        lease_until: r.get::<_, Option<Millis>>(10)?.map(|m| m.0),
        lease_seconds: r.get(11)?,
        claimed_by: r.get::<_, Option<Text<Emitter>>>(12)?.map(|e| e.0),
        lost_leases: r.get(13)?,
        steps: r.get(14)?,
        step: r.get(15)?,
        output: r.get(16)?,
        // A code without its message is a store that has failed.
        error: match r.get(17)? {
            Some(code) => Some(RunError {
                code,
                message: r.get(18)?,
            }),
            None => None,
        },
        ended_at: r.get::<_, Option<Millis>>(19)?.map(|m| m.0),
    })
}

impl ReadStore<'_> {
    /// The functions `tail` selects, in its order, each with its triggers in name order.
    fn functions(
        &self,
        tail: &str,
        values: impl rusqlite::Params,
    ) -> rusqlite::Result<Vec<Function>> {
        let sql = format!("SELECT {FUNCTION_COLUMNS} {tail}");
        let mut rows = self.0.prepare(&sql)?;
        let rows = rows
            .query_map(values, |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get::<_, Millis>(5)?.0,
                    r.get::<_, Text<Emitter>>(6)?.0,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut triggers = self
            .0
            .prepare("SELECT event_name FROM triggers WHERE function = ?1 ORDER BY event_name")?;
        rows.into_iter()
            .map(|(name, code, attempts, first, max, put_at, put_by)| {
                let triggers = triggers
                    .query_map([&name], |r| r.get(0))?
                    .collect::<rusqlite::Result<_>>()?;
                Ok(Function {
                    definition: Definition {
                        name,
                        code,
                        triggers,
                        max_attempts: attempts,
                        first_delay_seconds: first,
                        max_delay_seconds: max,
                    },
                    put_at,
                    put_by,
                })
            })
            .collect()
    }
}

impl ExecutionView for ReadStore<'_> {
    type Error = StoreError;

    fn function(&self, name: &str) -> Result<Option<Function>, StoreError> {
        let functions = self.functions("WHERE name = ?1", params![name]);
        store_err(functions.map(|f| f.into_iter().next()))
    }

    fn triggered_by(&self, event: &str, other_than: &str) -> Result<u64, StoreError> {
        let sql = "SELECT COUNT(*) FROM triggers WHERE event_name = ?1 AND function <> ?2";
        store_err(self.0.query_row(sql, [event, other_than], |r| r.get(0)))
    }

    fn function_count(&self) -> Result<u64, StoreError> {
        store_err(
            self.0
                .query_row("SELECT COUNT(*) FROM functions", [], |r| r.get(0)),
        )
    }

    fn functions_after(
        &self,
        after: Option<&str>,
        limit: u32,
    ) -> Result<Vec<Function>, StoreError> {
        // No name is empty, so "" stands for "from the first".
        let after = after.unwrap_or_default();
        let tail = "WHERE name > ?1 ORDER BY name LIMIT ?2";
        store_err(self.functions(tail, params![after, limit]))
    }

    fn run(&self, id: RunId) -> Result<Option<Run>, StoreError> {
        let sql = format!("SELECT {RUN_COLUMNS} WHERE id = ?1");
        store_err(self.0.query_row(&sql, [id.to_string()], row_run).optional())
    }

    fn step(&self, run: RunId, position: u32) -> Result<Step, StoreError> {
        let sql = format!("SELECT {STEP_COLUMNS} WHERE run_id = ?1 AND position = ?2");
        let step = self
            .0
            .query_row(&sql, params![run.to_string(), position], |r| {
                Ok(Step {
                    position: r.get(0)?,
                    name: r.get(1)?,
                    once: r.get(2)?,
                    input: r.get(3)?,
                    execution: r.get(4)?,
                    output: r.get(5)?,
                })
            });
        // No row: a step the run counts is lost, so the read store has failed.
        store_err(step)
    }

    fn last_run_sequence(&self) -> Result<u64, StoreError> {
        let sql = "SELECT COALESCE(MAX(sequence), 0) FROM runs";
        store_err(self.0.query_row(sql, [], |r| r.get(0)))
    }

    fn claimable(
        &self,
        pairs: &[(String, String)],
        now: Timestamp,
        limit: u32,
    ) -> Result<Vec<Run>, StoreError> {
        if pairs.is_empty() {
            return Ok(Vec::new());
        }
        let sql = claim_sql(pairs.len());
        let millis = now.as_millis();
        let mut values: Vec<&dyn ToSql> = vec![&millis, &limit];
        for (name, code) in pairs {
            values.push(name);
            values.push(code);
        }
        let runs = || -> rusqlite::Result<Vec<Run>> {
            let mut rows = self.0.prepare(&sql)?;
            rows.query_map(params_from_iter(values), row_run)?.collect()
        };
        store_err(runs())
    }
}
