//! The ledger: open, the writer, the read gate and close (architecture.md, "Commit and read"
//! and "Open").

use std::fmt;
use std::fs::File;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use ironflowx_core::{Clock, EnvRef, EventId, SequencedTime, Timestamp};
use ironflowx_events::{Error, ItemOutcome, Record, Refusal, Submission};
use rusqlite::Connection;
use segment_log_core::{
    AppendError, CommitError, ExpectedRevision, NewRecord, Options, SegmentLog, Submit,
};
use tokio::sync::{mpsc, oneshot, watch};

use crate::record::{self, FORMAT, LogRecord};
use crate::store::{self, ReadStore, StoreError};

const QUEUE_CAPACITY: usize = 16;
const STREAM: &[u8] = b"events";

/// One environment's commit log, read store and the single writer between them.
pub struct Ledger {
    env: EnvRef,
    // `Option`s so that `Drop` can close the queue, then join the writer.
    jobs: Option<mpsc::Sender<Job>>,
    writer: Option<thread::JoinHandle<()>>,
    progress: Arc<watch::Sender<Progress>>,
    pub(crate) store: Arc<Mutex<Connection>>,
    /// Set by `Drop`: the writer takes no further submission.
    stopping: Arc<AtomicBool>,
    #[cfg(any(test, feature = "__fault-points"))]
    faults: Arc<faults::Faults>,
    // Keep the lock until the writer has joined and the read store has closed.
    _directory_lock: File,
}

/// What a submission committed: one outcome per item, in input order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Committed {
    pub outcome: Vec<ItemOutcome>,
}

/// A submission failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubmitError {
    /// A single emit, or a whole batch, was refused by a rule.
    Refused(Refusal),
    /// The append's outcome is unknown; the ledger is now unavailable until reopened.
    Unknown,
    /// The ledger failed and refuses every call until it is reopened.
    Unavailable,
}

/// A read failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadError {
    Refused(Refusal),
    Unavailable,
}

/// Why a directory did not open.
#[derive(Debug)]
pub enum OpenError {
    /// Another process or `Ledger` holds the directory.
    Locked,
    /// The directory belongs to another environment ID or incarnation token.
    WrongIncarnation,
    /// The directory is in another format.
    Format,
    /// The log is damaged, holds a foreign record, or lost records the read store already
    /// folded. Needs a person: architecture.md, "When open answers `Corrupt`".
    Corrupt,
    Io(std::io::Error),
}

impl fmt::Display for OpenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Locked => f.write_str("the ledger directory is in use"),
            Self::WrongIncarnation => {
                f.write_str("the ledger directory belongs to another environment or incarnation")
            }
            Self::Format => f.write_str("the ledger directory is in another format"),
            Self::Corrupt => f.write_str("the ledger log is corrupt"),
            Self::Io(e) => write!(f, "ledger I/O: {e}"),
        }
    }
}

impl std::error::Error for OpenError {}

impl From<std::io::Error> for OpenError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// The read gate. Both positions are exclusive log offsets.
#[derive(Debug, Clone, Copy)]
struct Progress {
    committed: u64,
    applied: u64,
    failed: bool,
}

struct Job {
    sub: Submission,
    reply: oneshot::Sender<Result<Committed, SubmitError>>,
}

/// Mints a fresh environment reference: a UUIDv7 ID and a 128-bit token from the OS random
/// source.
///
/// # Panics
///
/// If the OS random source fails.
pub fn new_env_ref() -> EnvRef {
    let mut bits = [0; 16];
    getrandom::fill(&mut bits).expect("the OS random source works");
    format!(
        "{}.{:032x}",
        uuid::Uuid::now_v7(),
        u128::from_le_bytes(bits)
    )
    .parse()
    .expect("a UUIDv7 and 32 hex digits parse")
}

fn new_event_id() -> EventId {
    uuid::Uuid::now_v7()
        .to_string()
        .parse()
        .expect("a UUIDv7 parses")
}

impl Ledger {
    /// Opens the environment `env` in `dir`, creating it if `dir` is empty or missing.
    ///
    /// # Panics
    ///
    /// Outside a tokio runtime.
    pub async fn open(
        dir: &Path,
        env: EnvRef,
        clock: impl Clock + Send + 'static,
    ) -> Result<Self, OpenError> {
        let dir = dir.to_owned();
        let recovered = tokio::task::spawn_blocking(move || recover(&dir, env))
            .await
            .expect("open does not panic")?;
        let progress = Arc::new(watch::Sender::new(Progress {
            committed: recovered.watermark,
            applied: recovered.watermark,
            failed: false,
        }));
        let store = Arc::new(Mutex::new(recovered.conn));
        #[cfg(any(test, feature = "__fault-points"))]
        let faults = Arc::<faults::Faults>::default();
        let (jobs, queue) = mpsc::channel(QUEUE_CAPACITY);
        let stopping = Arc::<AtomicBool>::default();
        let writer = Writer {
            log: recovered.log,
            store: store.clone(),
            clock: Box::new(clock),
            env,
            last_time: recovered.last_time,
            revision: recovered.revision,
            progress: progress.clone(),
            stopping: stopping.clone(),
            #[cfg(any(test, feature = "__fault-points"))]
            faults: faults.clone(),
        };
        let writer = thread::Builder::new()
            .name("ironflowx-ledger-writer".into())
            .spawn(move || writer.run(queue))?;
        Ok(Self {
            env,
            _directory_lock: recovered.directory_lock,
            jobs: Some(jobs),
            writer: Some(writer),
            progress,
            store,
            stopping,
            #[cfg(any(test, feature = "__fault-points"))]
            faults,
        })
    }

    /// The environment this ledger was opened with.
    pub const fn env(&self) -> EnvRef {
        self.env
    }

    /// Commits a submission. Answers once its record is durable, or at once when nothing is
    /// new.
    pub async fn submit(&self, sub: Submission) -> Result<Committed, SubmitError> {
        if self.progress.borrow().failed {
            return Err(SubmitError::Unavailable);
        }
        let (reply, answer) = oneshot::channel();
        self.jobs
            .as_ref()
            .expect("set until drop")
            .send(Job { sub, reply })
            .await
            .map_err(|_| SubmitError::Unavailable)?;
        // Before append begins, losing the writer means this job did not commit.
        answer.await.unwrap_or(Err(SubmitError::Unavailable))
    }

    /// Runs a query from event rules once the read store holds every write acknowledged
    /// before this call.
    ///
    /// # Panics
    ///
    /// Outside a tokio runtime. A query that panics fails the ledger instead.
    pub async fn read<R: Send + 'static>(
        &self,
        q: impl FnOnce(&ReadStore<'_>) -> Result<R, Error<StoreError>> + Send + 'static,
    ) -> Result<R, ReadError> {
        let mut gate = self.progress.subscribe();
        let head = gate.borrow().committed;
        let ready = gate
            .wait_for(|p| p.failed || p.applied >= head)
            .await
            .is_ok_and(|p| !p.failed);
        if !ready {
            return Err(ReadError::Unavailable);
        }
        let store = self.store.clone();
        let answer = tokio::task::spawn_blocking(move || {
            // Poisoned: an earlier query panicked holding it.
            let conn = store.lock().ok()?;
            Some(q(&ReadStore(&conn)))
        })
        .await;
        match answer {
            Ok(Some(Ok(r))) => Ok(r),
            Ok(Some(Err(Error::Refused(r)))) => Err(ReadError::Refused(r)),
            // A store error, a poisoned store or a panicking query.
            _ => {
                fail(&self.progress);
                Err(ReadError::Unavailable)
            }
        }
    }

    /// Finishes queued submissions and the fold, then releases the directory.
    ///
    /// # Panics
    ///
    /// Outside a tokio runtime.
    pub async fn close(mut self) {
        #[cfg(any(test, feature = "__fault-points"))]
        self.faults.release_fold();
        let writer = self.writer.take();
        drop(self.jobs.take());
        tokio::task::spawn_blocking(move || {
            if let Some(writer) = writer {
                let _ = writer.join();
            }
            drop(self);
        })
        .await
        .expect("drop does not panic");
    }
}

/// Without `close`, dropping stops the writer after the submission in hand: queued ones, never
/// decided, end `Unavailable`. It joins the writer thread, so it blocks until that submission's
/// fsync and fold finish, and releases the directory before it returns. From async code, call
/// [`Ledger::close`] instead.
impl Drop for Ledger {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::SeqCst);
        #[cfg(any(test, feature = "__fault-points"))]
        self.faults.release_fold();
        drop(self.jobs.take());
        if let Some(writer) = self.writer.take() {
            let _ = writer.join();
        }
    }
}

fn fail(progress: &watch::Sender<Progress>) {
    progress.send_modify(|p| p.failed = true);
}

/// Losing the reply during an append leaves its outcome uncertain.
struct AppendReply(Option<oneshot::Sender<Result<Committed, SubmitError>>>);

impl AppendReply {
    fn send(mut self, result: Result<Committed, SubmitError>) {
        let _ = self.0.take().expect("reply is pending").send(result);
    }
}

impl Drop for AppendReply {
    fn drop(&mut self) {
        if let Some(reply) = self.0.take() {
            let _ = reply.send(Err(SubmitError::Unknown));
        }
    }
}

struct Writer {
    log: SegmentLog,
    store: Arc<Mutex<Connection>>,
    clock: Box<dyn Clock + Send>,
    env: EnvRef,
    /// The last committed event time; `None` before the first record.
    last_time: Option<Timestamp>,
    revision: u64,
    progress: Arc<watch::Sender<Progress>>,
    stopping: Arc<AtomicBool>,
    #[cfg(any(test, feature = "__fault-points"))]
    faults: Arc<faults::Faults>,
}

/// However the writer ends, a panic included, the ledger is failed, so no read waits forever.
struct FailOnExit(Arc<watch::Sender<Progress>>);

impl Drop for FailOnExit {
    fn drop(&mut self) {
        fail(&self.0);
    }
}

impl Writer {
    /// One submission at a time: decide, append, fsync, publish, acknowledge, fold (D15).
    fn run(mut self, mut queue: mpsc::Receiver<Job>) {
        let _exit = FailOnExit(self.progress.clone());
        while let Some(job) = queue.blocking_recv() {
            if self.stopping.load(Ordering::SeqCst) || self.progress.borrow().failed {
                let _ = job.reply.send(Err(SubmitError::Unavailable));
                continue;
            }
            let decision = match self.decide(&job.sub) {
                Ok(decision) => decision,
                Err(e) => {
                    let _ = job.reply.send(Err(e));
                    continue;
                }
            };
            let answer = Ok(Committed {
                outcome: decision.outcome,
            });
            let Some(record) = decision.record else {
                let _ = job.reply.send(answer);
                continue;
            };
            let payload = record::events(self.env.token(), &record);
            let reply = AppendReply(Some(job.reply));
            let offset = match self.append(payload) {
                Ok(offset) => offset,
                Err(e) => {
                    fail(&self.progress);
                    reply.send(Err(e));
                    continue;
                }
            };
            self.last_time = Some(record.time);
            #[cfg(feature = "__fault-points")]
            point::reach(point::FaultPoint::AfterFsync);
            self.progress.send_modify(|p| p.committed = offset + 1);
            reply.send(answer);
            #[cfg(feature = "__fault-points")]
            point::reach(point::FaultPoint::AfterAck);
            self.fold(offset, &record);
        }
    }

    fn decide(&self, sub: &Submission) -> Result<ironflowx_events::Decision, SubmitError> {
        // D7: the event clock never goes back.
        let now = self.clock.now();
        let now = self.last_time.map_or(now, |t| t.max(now));
        let decision = match self.store.lock() {
            Ok(conn) => ironflowx_events::decide(
                &ReadStore(&conn),
                sub,
                SequencedTime::from_event_clock(now),
                &mut std::iter::repeat_with(new_event_id),
            ),
            // Poisoned: a query panicked holding it.
            Err(_) => {
                fail(&self.progress);
                return Err(SubmitError::Unavailable);
            }
        };
        decision.map_err(|e| match e {
            Error::Refused(r) => SubmitError::Refused(r),
            Error::Store(_) => {
                fail(&self.progress);
                SubmitError::Unavailable
            }
        })
    }

    /// Appends one record and waits for its fsync. No retry (D11).
    fn append(&mut self, payload: Vec<u8>) -> Result<u64, SubmitError> {
        #[cfg(feature = "__fault-points")]
        point::reach(point::FaultPoint::BeforeAppend);
        #[cfg(any(test, feature = "__fault-points"))]
        let fault = self.faults.take_append();
        #[cfg(any(test, feature = "__fault-points"))]
        if fault == Some(faults::AppendFault::Lost) {
            return Err(SubmitError::Unknown);
        }
        let position = append(&self.log, ExpectedRevision::At(self.revision), payload)?;
        #[cfg(any(test, feature = "__fault-points"))]
        if fault == Some(faults::AppendFault::Landed) {
            return Err(SubmitError::Unknown);
        }
        self.revision = position.revision;
        Ok(position.offset)
    }

    fn fold(&self, offset: u64, record: &Record) {
        #[cfg(any(test, feature = "__fault-points"))]
        {
            self.faults.wait_while_held();
            if self.faults.take_fold() {
                fail(&self.progress);
                return;
            }
        }
        let Ok(mut conn) = self.store.lock() else {
            fail(&self.progress);
            return;
        };
        match store::apply(&mut conn, offset, Some(record), false) {
            Ok(()) => self.progress.send_modify(|p| p.applied = offset + 1),
            Err(_) => fail(&self.progress),
        }
    }
}

fn append(
    log: &SegmentLog,
    expected: ExpectedRevision,
    payload: Vec<u8>,
) -> Result<segment_log_core::Position, SubmitError> {
    let pending = log
        .append(Submit {
            records: vec![NewRecord {
                stream: STREAM.to_vec(),
                expected,
                key: None,
                payload,
            }],
        })
        // Every other append error cannot happen by construction: a fault all the same.
        .map_err(|_: AppendError| SubmitError::Unavailable)?;
    match pending.wait() {
        Ok(mut c) => Ok(c.records.remove(0)),
        Err(CommitError::Indeterminate) => Err(SubmitError::Unknown),
        Err(_) => Err(SubmitError::Unavailable),
    }
}

struct Recovered {
    directory_lock: File,
    log: SegmentLog,
    conn: Connection,
    watermark: u64,
    last_time: Option<Timestamp>,
    revision: u64,
}

/// Create missing components from the top down, syncing each parent before
/// creating anything inside its child. Existing directories need only traversal
/// permissions, including when the embedder supplies a relative path.
fn create_directory(dir: &Path) -> std::io::Result<()> {
    let absolute = std::path::absolute(dir)?;
    let mut missing = Vec::new();
    for path in absolute.ancestors() {
        if path.try_exists()? {
            break;
        }
        missing.push(path);
    }
    for path in missing.into_iter().rev() {
        let parent = File::open(path.parent().expect("root exists"))?;
        match std::fs::create_dir(path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && path.is_dir() => {}
            Err(e) => return Err(e),
        }
        parent.sync_all()?;
    }
    Ok(())
}

/// Take the ledger lock before opening either store.
fn recover(dir: &Path, env: EnvRef) -> Result<Recovered, OpenError> {
    create_directory(dir)?;
    // The segment log releases its own lock when sealed. Keep exclusive ownership
    // of the read store through recovery, normal operation and close.
    let directory_lock = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join("ledger.lock"))?;
    directory_lock.try_lock().map_err(|e| match e {
        std::fs::TryLockError::WouldBlock => OpenError::Locked,
        std::fs::TryLockError::Error(e) => OpenError::Io(e),
    })?;
    let options = Options {
        linger: std::time::Duration::ZERO,
        group_byte_cap: 8 * 1024 * 1024,
        pending_byte_budget: 16 * 1024 * 1024,
        ..Options::default()
    };
    let log = SegmentLog::open(dir.join("log"), options).map_err(|e| {
        use segment_log_core::OpenError as E;
        match e {
            E::Locked => OpenError::Locked,
            E::UnsupportedVersion(_) => OpenError::Format,
            E::Corrupt { .. } | E::LogIdMismatch { .. } | E::Gap { .. } | E::NotSealed => {
                OpenError::Corrupt
            }
            E::Io(e) => OpenError::Io(e),
            e @ (E::Exhausted | E::InvalidOptions(_)) => {
                OpenError::Io(std::io::Error::other(format!("{e:?}")))
            }
        }
    })?;
    // The fold runs only after fsync, so a read store of this environment ahead of the log
    // proves the log lost durable records. Refuse, before a genesis could paper over it.
    if store::recorded(dir, env, log.watermark() == 0)?
        .is_some_and(|applied| applied > log.watermark())
    {
        return Err(OpenError::Corrupt);
    }
    if log.watermark() == 0 {
        append(&log, ExpectedRevision::Empty, record::genesis(env))
            .map_err(|e| OpenError::Io(std::io::Error::other(format!("genesis: {e:?}"))))?;
    }
    match read_record(&log, 0)? {
        LogRecord::Genesis { format, .. } if format != FORMAT => return Err(OpenError::Format),
        LogRecord::Genesis { env: genesis, .. } if genesis != env => {
            return Err(OpenError::WrongIncarnation);
        }
        LogRecord::Genesis { .. } => {}
        LogRecord::Events { .. } => return Err(OpenError::Corrupt),
    }
    let watermark = log.watermark();
    let (conn, last_time) = match replay(&log, store::open(dir, env, watermark, false)?, env)? {
        Ok(recovered) => recovered,
        // `meta` agreed but the tables did not: rebuild from the log, once.
        Err(_) => replay(&log, store::open(dir, env, watermark, true)?, env)??,
    };
    let revision = log.stream_head(STREAM).ok_or(OpenError::Corrupt)?.revision;
    Ok(Recovered {
        directory_lock,
        log,
        conn,
        watermark,
        last_time,
        revision,
    })
}

/// Folds every record from the read store's applied position to the log's watermark. The
/// outer error is the log's; the inner, the read store's.
fn replay(
    log: &SegmentLog,
    mut conn: Connection,
    env: EnvRef,
) -> Result<std::io::Result<(Connection, Option<Timestamp>)>, OpenError> {
    let mut from = match store::applied(&conn) {
        Ok(applied) => applied,
        Err(e) => return Ok(Err(std::io::Error::other(e))),
    };
    // Every non-genesis record written by the ledger ends with a freshly minted
    // event ID. It identifies this prefix even across forks of the same incarnation.
    let mut last_time = None;
    let expected = if from > 1 {
        let record = read_events(log, from - 1, env)?;
        last_time = Some(record.time);
        let event = record.events.last().ok_or(OpenError::Corrupt)?;
        Some((event.sequence, event.id))
    } else {
        None
    };
    match store::last_event(&conn) {
        Ok(actual) if actual == expected => {}
        Ok(_) => return Ok(Err(std::io::Error::other("read store diverges from log"))),
        Err(e) => return Ok(Err(std::io::Error::other(e))),
    }
    while from < log.watermark() {
        let record = if from == 0 {
            None
        } else {
            let record = read_events(log, from, env)?;
            last_time = Some(record.time);
            Some(record)
        };
        if let Err(e) = store::apply(&mut conn, from, record.as_ref(), true) {
            return Ok(Err(std::io::Error::other(e)));
        }
        from += 1;
    }
    Ok(Ok((conn, last_time)))
}

/// A later record: it must carry this incarnation's token and format (LED-8).
fn read_events(log: &SegmentLog, offset: u64, env: EnvRef) -> Result<Record, OpenError> {
    match read_record(log, offset)? {
        LogRecord::Events {
            format,
            token,
            record,
        } if format == FORMAT && token == env.token() => Ok(record),
        _ => Err(OpenError::Corrupt),
    }
}

fn read_record(log: &SegmentLog, offset: u64) -> Result<LogRecord, OpenError> {
    let records = log.read(offset, 1).map_err(log_read_error)?;
    let payload = &records.first().ok_or(OpenError::Corrupt)?.payload;
    // A genesis in another format may not decode as this one: check the format first.
    if offset == 0
        && let [0, lo, hi, ..] = payload[..]
        && u16::from_le_bytes([lo, hi]) != FORMAT
    {
        return Err(OpenError::Format);
    }
    record::decode(payload).ok_or(OpenError::Corrupt)
}

fn log_read_error(e: segment_log_core::ReadError) -> OpenError {
    match e {
        segment_log_core::ReadError::Corrupt { .. } => OpenError::Corrupt,
        segment_log_core::ReadError::Io(e) => OpenError::Io(e),
    }
}

/// The crash test's named kill points (#6, D23). The hook is process-wide, so a point inside open
/// needs no `Ledger` to reach it.
#[cfg(feature = "__fault-points")]
pub mod point {
    use std::sync::OnceLock;

    /// A place in the commit path where the crash test kills the process.
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
            }
        }
    }

    static HOOK: OnceLock<fn(FaultPoint)> = OnceLock::new();

    /// Calls `hook` on the thread that reaches each point, before the point's work. The
    /// first hook set stays.
    pub fn set_fault_hook(hook: fn(FaultPoint)) {
        let _ = HOOK.set(hook);
    }

    pub(crate) fn reach(point: FaultPoint) {
        if let Some(hook) = HOOK.get() {
            hook(point);
        }
    }
}

#[cfg(any(test, feature = "__fault-points"))]
mod faults {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Condvar, Mutex};

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum AppendFault {
        /// The record is not appended.
        Lost,
        /// The record is appended and durable.
        Landed,
    }

    #[derive(Default)]
    pub struct Faults {
        held: Mutex<bool>,
        released: Condvar,
        append: Mutex<Option<AppendFault>>,
        fold: AtomicBool,
    }

    impl Faults {
        pub fn hold_fold(&self) {
            *self.held.lock().unwrap() = true;
        }

        pub fn release_fold(&self) {
            *self.held.lock().unwrap() = false;
            self.released.notify_all();
        }

        pub fn wait_while_held(&self) {
            let held = self.held.lock().unwrap();
            drop(self.released.wait_while(held, |h| *h).unwrap());
        }

        pub fn fail_append(&self, fault: AppendFault) {
            *self.append.lock().unwrap() = Some(fault);
        }

        pub fn take_append(&self) -> Option<AppendFault> {
            self.append.lock().unwrap().take()
        }

        pub fn fail_fold(&self) {
            self.fold.store(true, Ordering::SeqCst);
        }

        pub fn take_fold(&self) -> bool {
            self.fold.swap(false, Ordering::SeqCst)
        }
    }
}

/// Test hooks, behind the internal `__fault-points` feature.
#[cfg(any(test, feature = "__fault-points"))]
impl Ledger {
    /// Holds the writer before its next fold, until [`Ledger::release_fold`].
    pub fn hold_fold(&self) {
        self.faults.hold_fold();
    }

    pub fn release_fold(&self) {
        self.faults.release_fold();
    }

    /// The next append answers `Unknown`. With `landed`, its record is appended and durable
    /// first; without, it is not appended.
    pub fn fail_next_append(&self, landed: bool) {
        self.faults.fail_append(if landed {
            faults::AppendFault::Landed
        } else {
            faults::AppendFault::Lost
        });
    }

    /// The next fold fails, after its submission is acknowledged.
    pub fn fail_next_fold(&self) {
        self.faults.fail_fold();
    }
}
