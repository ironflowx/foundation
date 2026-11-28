//! The ledger: open, the writer, the read gate and close (architecture.md, "Commit and read"
//! and "Open").

use std::fmt;
use std::fs::File;
use std::num::NonZeroU32;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use ironflowx_core::{Clock, Emitter, EnvRef, EventId, RunId, SequencedTime, SubmitAs, Timestamp};
use ironflowx_events::{Error, Event, EventPage, ItemOutcome, Record, Refusal};
use ironflowx_execution::{Command, Draws, Function, FunctionPage, Run};
use rusqlite::Connection;
use segment_log_core::{
    AppendError, CommitError, ExpectedRevision, NewRecord, Options, SegmentLog, Submit,
};
use tokio::sync::{mpsc, oneshot, watch};

use crate::record::{self, Entries, FORMAT};
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
    _directory_lock: DirectoryLock,
}

/// What a caller asks the ledger to commit, of either rule module (D112).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Submission {
    /// An emit or a batch, decided by event rules.
    Events(ironflowx_events::Submission),
    /// A command, decided by execution rules.
    Execution(Command),
}

/// What a submission committed, or would have: the answer of the rule module that decided it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// One outcome per item, in input order.
    Events(Vec<ItemOutcome>),
    Execution(ironflowx_execution::Outcome),
}

/// What a submission committed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Committed {
    pub outcome: Outcome,
}

/// A submission failed.
/// Its `Display` shows a refusal's message, and it has no `source()`, so a reporter that walks the
/// chain shows the cause once (D84).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubmitError {
    /// A rule refused the submission: a single emit, a whole batch, or a command.
    Refused(Refusal),
    /// The append's outcome is unknown; the ledger is now unavailable until reopened.
    Unknown,
    /// The ledger failed and refuses every call until it is reopened.
    Unavailable,
}

/// A read failed.
/// Its `Display` shows a refusal's message, and it has no `source()`, so a reporter that walks the
/// chain shows the cause once (D84).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadError {
    /// A query refused the request: an unknown event, function or run, or a bad cursor.
    Refused(Refusal),
    /// The ledger failed and refuses every call until it is reopened.
    Unavailable,
}

const UNAVAILABLE: &str = "the ledger is unavailable until it is reopened";

impl fmt::Display for SubmitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused(r) => r.fmt(f),
            Self::Unknown => write!(f, "the submission's outcome is unknown, and {UNAVAILABLE}"),
            Self::Unavailable => f.write_str(UNAVAILABLE),
        }
    }
}

impl std::error::Error for SubmitError {}

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused(r) => r.fmt(f),
            Self::Unavailable => f.write_str(UNAVAILABLE),
        }
    }
}

impl std::error::Error for ReadError {}

/// Whether an open ledger has failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LedgerState {
    Open,
    /// It failed and refuses every call until it is reopened.
    Unavailable,
}

/// A ledger failed while it was open (a ledger fault): it refuses every call until it is reopened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fault;

/// Why a directory did not open.
/// Its `Display` shows the inner error, and it has no `source()`, so a reporter that walks the
/// chain shows the cause once (D84).
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
    /// An open of an existing ledger found none, or one that begins with a record of events.
    Missing,
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
            Self::Missing => f.write_str("the ledger is missing"),
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

/// The read gate. Both positions are exclusive: one past the last record.
#[derive(Debug, Clone, Copy)]
struct Progress {
    committed: u64,
    applied: u64,
    /// The event time of the last applied record; `None` before the first.
    time: Option<Timestamp>,
    failed: bool,
    /// The writer has joined: no failure counts from here on.
    closed: bool,
}

struct Job {
    sub: Submission,
    by: SubmitAs,
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

fn new_run_id() -> RunId {
    uuid::Uuid::now_v7()
        .to_string()
        .parse()
        .expect("a UUIDv7 parses")
}

impl Ledger {
    /// Opens the environment `env` in `dir`, creating it if `dir` is missing or holds neither a log
    /// nor `read.db`, whatever else it holds: an engine's root is not recognised.
    ///
    /// # Panics
    ///
    /// Outside a tokio runtime, or on one that has shut down.
    pub async fn open(
        dir: &Path,
        env: EnvRef,
        clock: impl Clock + Send + 'static,
    ) -> Result<Self, OpenError> {
        let dir = dir.to_owned();
        tokio::task::spawn_blocking(move || Self::open_blocking(&dir, env, clock))
            .await
            .expect("open does not panic")
    }

    /// As [`Ledger::open`], on this thread and outside a runtime too.
    pub fn open_blocking(
        dir: &Path,
        env: EnvRef,
        clock: impl Clock + Send + 'static,
    ) -> Result<Self, OpenError> {
        create_directory(dir)?;
        Self::start(recover(dir, env, false)?, env, clock)
    }

    /// As [`Ledger::open_blocking`], but refuses as `Missing` a directory that is missing, or whose
    /// log is missing, empty or begins with a record of events: it creates neither a directory nor
    /// a genesis record. A log that has a segment is opened to find out.
    pub fn open_existing_blocking(
        dir: &Path,
        env: EnvRef,
        clock: impl Clock + Send + 'static,
    ) -> Result<Self, OpenError> {
        if !holds_segment(dir)? {
            return Err(OpenError::Missing);
        }
        Self::start(recover(dir, env, true)?, env, clock)
    }

    fn start(
        recovered: Recovered,
        env: EnvRef,
        clock: impl Clock + Send + 'static,
    ) -> Result<Self, OpenError> {
        let progress = Arc::new(watch::Sender::new(Progress {
            committed: recovered.watermark,
            applied: recovered.watermark,
            time: recovered.last_time,
            failed: false,
            closed: false,
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

    /// Whether the ledger has failed.
    pub fn state(&self) -> LedgerState {
        if self.progress.borrow().failed {
            LedgerState::Unavailable
        } else {
            LedgerState::Open
        }
    }

    /// Resolves once, when the ledger fails, with a call in flight or none; at once if it has
    /// failed already.
    ///
    /// A close is no fault. What is pending when [`Ledger::close`] begins resolves only if the
    /// ledger fails before `close` returns, as by the fold it finishes; after that it stays pending
    /// for good. The same for a drop.
    pub fn faulted(&self) -> impl Future<Output = Fault> + Send + 'static + use<> {
        let mut gate = self.progress.subscribe();
        async move {
            // `wait_for` answers later when its task's budget is spent.
            let failed = gate.borrow().failed;
            if !failed && gate.wait_for(|p| p.failed).await.is_err() {
                // Closed, and never failed.
                std::future::pending::<()>().await;
            }
            Fault
        }
    }

    /// Commits a submission attributed to `by`. Answers once its record is durable, or at once
    /// when nothing is new. The ledger trusts its caller's attribution (D36).
    pub async fn submit(&self, sub: Submission, by: SubmitAs) -> Result<Committed, SubmitError> {
        if self.progress.borrow().failed {
            return Err(SubmitError::Unavailable);
        }
        let (reply, answer) = oneshot::channel();
        self.jobs
            .as_ref()
            .expect("set until drop")
            .send(Job { sub, by, reply })
            .await
            .map_err(|_| SubmitError::Unavailable)?;
        // Before append begins, losing the writer means this job did not commit.
        answer.await.unwrap_or(Err(SubmitError::Unavailable))
    }

    /// The event with this ID, once the read store holds every write acknowledged before this
    /// call.
    ///
    /// # Errors
    ///
    /// `Refused` with `NotFound` for an unknown or malformed ID; `Unavailable` if the ledger
    /// failed.
    ///
    /// # Panics
    ///
    /// Outside a tokio runtime.
    pub async fn get_event(&self, id: &str) -> Result<Event, ReadError> {
        let id = id.to_owned();
        self.read(move |v| ironflowx_events::get_event(v, &id))
            .await
    }

    /// A page of events, newest first, once the read store holds every write acknowledged
    /// before this call. `limit` is 20 when `None` and at most 200; `cursor` is a page's `next`
    /// from this ledger.
    ///
    /// # Errors
    ///
    /// `Refused` with `Invalid` for a cursor that does not decode or names another incarnation;
    /// a well-formed cursor of this incarnation is not checked further. `Unavailable` if the
    /// ledger failed.
    ///
    /// # Panics
    ///
    /// Outside a tokio runtime.
    pub async fn list_events(
        &self,
        limit: Option<NonZeroU32>,
        cursor: Option<&str>,
    ) -> Result<EventPage, ReadError> {
        let (token, cursor) = (self.env.token(), cursor.map(str::to_owned));
        self.read(move |v| ironflowx_events::list_events(v, token, limit, cursor.as_deref()))
            .await
    }

    /// The function of this name, once the read store holds every write acknowledged before this
    /// call.
    ///
    /// # Errors
    ///
    /// `Refused` with `NotFound` for an unknown name; `Unavailable` if the ledger failed.
    ///
    /// # Panics
    ///
    /// Outside a tokio runtime.
    pub async fn get_function(&self, name: &str) -> Result<Function, ReadError> {
        let name = name.to_owned();
        self.read(move |v| ironflowx_execution::get_function(v, &name))
            .await
    }

    /// A page of functions, in name order, once the read store holds every write acknowledged
    /// before this call. `limit` is 20 when `None` and at most 200; `cursor` is a page's `next`
    /// from this ledger.
    ///
    /// # Errors
    ///
    /// `Refused` with `Invalid` for a cursor that does not decode or names another incarnation;
    /// `Unavailable` if the ledger failed.
    ///
    /// # Panics
    ///
    /// Outside a tokio runtime.
    pub async fn list_functions(
        &self,
        limit: Option<NonZeroU32>,
        cursor: Option<&str>,
    ) -> Result<FunctionPage, ReadError> {
        let (token, cursor) = (self.env.token(), cursor.map(str::to_owned));
        self.read(move |v| ironflowx_execution::list_functions(v, token, limit, cursor.as_deref()))
            .await
    }

    /// The run with this ID, once the read store holds every write acknowledged before this call.
    ///
    /// # Errors
    ///
    /// `Refused` with `NotFound` for an unknown or malformed ID; `Unavailable` if the ledger
    /// failed.
    ///
    /// # Panics
    ///
    /// Outside a tokio runtime.
    pub async fn get_run(&self, id: &str) -> Result<Run, ReadError> {
        let id = id.to_owned();
        self.read(move |v| ironflowx_execution::get_run(v, &id))
            .await
    }

    /// What a waiting claim watches: it wakes once each commit is folded, and when the ledger
    /// fails. Taken before a read, it misses nothing folded after that read's snapshot.
    pub fn changes(&self) -> Changes {
        let rx = self.progress.subscribe();
        let seen = rx.borrow().applied;
        Changes { rx, seen }
    }

    /// How long until a run that `claim` may take is due, by the event clock, in the read store as
    /// it is: zero when one is due now, `None` when no run of its pairs is queued. It waits for no
    /// fold: a commit it does not see is folded later, and `changes` tells of it. The claim is
    /// checked as a submission's is. `clock` is the one the ledger was opened with: it is read
    /// after the snapshot, and the event time is the later of it and the snapshot's last record
    /// (D7), as a submission's is.
    ///
    /// # Errors
    ///
    /// `Refused` with `Invalid` for a claim outside the rules, or a command that is no claim;
    /// `Unavailable` if the ledger failed.
    ///
    /// # Panics
    ///
    /// Outside a tokio runtime.
    pub async fn claimable(
        &self,
        claim: &Command,
        clock: &(dyn Clock + Sync),
    ) -> Result<Option<Duration>, ReadError> {
        let pairs = ironflowx_execution::claim_pairs(claim).map_err(ReadError::Refused)?;
        let progress = self.progress.clone();
        let (due, last) = self
            .query(move |v| {
                let due = v.earliest_due(&pairs).map_err(Error::Store)?;
                // The fold moves it holding the store's lock, which this read holds.
                Ok((due, progress.borrow().time))
            })
            .await?;
        let clock = clock.now();
        let now = last.map_or(clock, |t| t.max(clock));
        Ok(due.map(|due| {
            if due <= now {
                return Duration::ZERO;
            }
            // Behind the last record, event time stands still until the clock passes it, or a
            // commit, which wakes the claim, moves it: the distance is the clock's.
            let millis = due.as_millis().saturating_sub(clock.as_millis());
            Duration::from_millis(u64::try_from(millis).unwrap_or(0))
        }))
    }

    /// Runs a query of either rule module once the read store holds every write acknowledged
    /// before this call.
    ///
    /// # Panics
    ///
    /// Outside a tokio runtime. A query that panics fails the ledger instead.
    pub(crate) async fn read<R: Send + 'static>(
        &self,
        q: impl FnOnce(&ReadStore<'_>) -> Result<R, Error<StoreError>> + Send + 'static,
    ) -> Result<R, ReadError> {
        let mut gate = self.progress.subscribe();
        let head = gate.borrow().committed;
        let ready = {
            let wait = gate.wait_for(|p| p.failed || p.applied >= head);
            #[cfg(feature = "__fault-points")]
            let wait = {
                let mut wait = std::pin::pin!(wait);
                let mut announced = false;
                std::future::poll_fn(move |cx| {
                    let polled = std::future::Future::poll(wait.as_mut(), cx);
                    if polled.is_pending() && !announced {
                        announced = true;
                        ironflowx_core::reach_fault_point(
                            ironflowx_core::FaultPoint::ReadWaitingForFold,
                        );
                    }
                    polled
                })
                .await
            };
            #[cfg(not(feature = "__fault-points"))]
            let wait = wait.await;
            wait.is_ok_and(|p| !p.failed)
        };
        if !ready {
            return Err(ReadError::Unavailable);
        }
        self.query(q).await
    }

    /// Runs a query on the read store as it is, folds still to come or not; `Unavailable` if the
    /// ledger has failed.
    async fn query<R: Send + 'static>(
        &self,
        q: impl FnOnce(&ReadStore<'_>) -> Result<R, Error<StoreError>> + Send + 'static,
    ) -> Result<R, ReadError> {
        if self.progress.borrow().failed {
            return Err(ReadError::Unavailable);
        }
        let store = self.store.clone();
        let gate = self.progress.clone();
        let answer = tokio::task::spawn_blocking(move || {
            let _on_panic = FailOnPanic(gate.clone());
            // Poisoned: an earlier query panicked holding it.
            let answer = store.lock().ok().map(|conn| q(&ReadStore(&conn)));
            match answer {
                Some(Ok(r)) => Ok(r),
                Some(Err(Error::Refused(r))) => Err(ReadError::Refused(r)),
                // Here and not below, which an abandoned read never reaches.
                Some(Err(Error::Store(_))) | None => {
                    fail(&gate);
                    Err(ReadError::Unavailable)
                }
            }
        })
        .await;
        answer.unwrap_or_else(|_| {
            fail(&self.progress);
            Err(ReadError::Unavailable)
        })
    }

    /// Finishes queued submissions and the fold, then releases the directory. What
    /// [`Ledger::faulted`] gave stays pending, unless the ledger has failed or does before this
    /// returns.
    ///
    /// # Panics
    ///
    /// Outside a tokio runtime, or on one that has shut down: the ledger is dropped then, its queue
    /// already ended.
    pub async fn close(self) {
        self.close_with_state().await;
    }

    /// Closes as [`Self::close`] does and returns the state at the start of close.
    ///
    /// The first read of the progress gate orders the start of close and fault publication.
    /// A fault published later still resolves [`Self::faulted`] while the ledger is closing.
    ///
    /// # Panics
    ///
    /// As [`Self::close`].
    pub async fn close_with_state(mut self) -> LedgerState {
        let at_start = self.state();
        self.end_queue();
        // The closure owns the writer: one that never runs drops the ledger, which joins it.
        tokio::task::spawn_blocking(move || self.join())
            .await
            .expect("the runtime has not shut down");
        at_start
    }

    /// Ends the queue, so that the writer ends.
    fn end_queue(&mut self) {
        #[cfg(any(test, feature = "__fault-points"))]
        self.faults.release_fold();
        drop(self.jobs.take());
    }

    /// Waits for the writer. A panic of its own teardown fails the ledger.
    fn join(&mut self) {
        if let Some(writer) = self.writer.take()
            && writer.join().is_err()
        {
            fail(&self.progress);
        }
    }
}

/// Without `close`, dropping stops the writer after the submission in hand: queued ones, never
/// decided, end `Unavailable`. It joins the writer thread, so it blocks until that submission's
/// fsync and fold finish, and releases the directory before it returns. From async code, call
/// [`Ledger::close`] instead.
impl Drop for Ledger {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::SeqCst);
        self.end_queue();
        self.join();
        self.progress.send_modify(|p| p.closed = true);
    }
}

/// A ledger's commits and failure, from [`Ledger::changes`].
pub struct Changes {
    rx: watch::Receiver<Progress>,
    /// The folded position it last told of.
    seen: u64,
}

impl Changes {
    /// Resolves at the first fold it has not told of since it was taken, so a fold tells once, and
    /// at once from a failure on, which stays; never once the ledger has closed, a close being no
    /// change.
    pub async fn changed(&mut self) {
        let seen = self.seen;
        let told = self.rx.wait_for(|p| p.failed || p.applied != seen).await;
        match told.map(|p| (p.failed, p.closed, p.applied)) {
            Ok((failed, closed, applied)) if failed || !closed => self.seen = applied,
            // Closed, and not failed: no change comes.
            _ => std::future::pending::<()>().await,
        }
    }
}

/// Fails the ledger, unless it has closed: a query its caller abandoned may outlive it.
fn fail(progress: &watch::Sender<Progress>) {
    progress.send_modify(|p| p.failed |= !p.closed);
}

/// Losing the reply during an append leaves its outcome uncertain.
struct AppendReply(Option<oneshot::Sender<Result<Committed, SubmitError>>>);

impl AppendReply {
    fn send(mut self, result: Result<Committed, SubmitError>) {
        #[cfg(feature = "__fault-points")]
        let ok = result.is_ok();
        let _ = self.0.take().expect("reply is pending").send(result);
        #[cfg(feature = "__fault-points")]
        if ok {
            ironflowx_core::reach_fault_point(ironflowx_core::FaultPoint::AfterAck);
        }
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

/// A panic while this is held fails the ledger: no read waits forever, and the fault is signalled.
struct FailOnPanic(Arc<watch::Sender<Progress>>);

impl Drop for FailOnPanic {
    fn drop(&mut self) {
        if thread::panicking() {
            fail(&self.0);
        }
    }
}

impl Writer {
    /// One submission at a time: decide, append, fsync, publish, acknowledge, fold (D15).
    fn run(mut self, mut queue: mpsc::Receiver<Job>) {
        while let Some(job) = queue.blocking_recv() {
            // After `job`, and again after `reply`, so that it drops first: a caller whose reply a
            // panic loses finds the ledger failed.
            let _on_panic = FailOnPanic(self.progress.clone());
            if self.stopping.load(Ordering::SeqCst) || self.progress.borrow().failed {
                let _ = job.reply.send(Err(SubmitError::Unavailable));
                continue;
            }
            let emitted_by = Emitter::from(job.by);
            let (outcome, entries) = match self.decide(&job.sub, &emitted_by) {
                Ok(decided) => decided,
                Err(e) => {
                    let _ = job.reply.send(Err(e));
                    continue;
                }
            };
            let answer = Ok(Committed { outcome });
            let Some(entries) = entries else {
                let _ = job.reply.send(answer);
                continue;
            };
            let id = uuid::Uuid::now_v7();
            let payload = record::entries(self.env.token(), id, &emitted_by, &entries);
            let reply = AppendReply(Some(job.reply));
            let _on_panic = FailOnPanic(self.progress.clone());
            let offset = match self.append(payload) {
                Ok(offset) => offset,
                Err(e) => {
                    fail(&self.progress);
                    reply.send(Err(e));
                    continue;
                }
            };
            self.last_time = Some(entries.record.time);
            #[cfg(feature = "__fault-points")]
            ironflowx_core::reach_fault_point(ironflowx_core::FaultPoint::AfterFsync);
            self.progress.send_modify(|p| p.committed = offset + 1);
            reply.send(answer);
            self.fold(offset, id, &entries, &emitted_by);
        }
    }

    /// Decides a submission: its answer, and what to append, if anything. One read-store lock
    /// spans the decision, so nothing commits between its reads, the engine events' sequences
    /// included.
    fn decide(
        &self,
        sub: &Submission,
        by: &Emitter,
    ) -> Result<(Outcome, Option<Entries>), SubmitError> {
        // D7: the event clock never goes back.
        let now = self.clock.now();
        let now = self.last_time.map_or(now, |t| t.max(now));
        let Ok(conn) = self.store.lock() else {
            // Poisoned: a query panicked holding it.
            fail(&self.progress);
            return Err(SubmitError::Unavailable);
        };
        let view = ReadStore(&conn);
        let fresh = &mut std::iter::repeat_with(new_event_id);
        let decision = match sub {
            Submission::Events(sub) => {
                ironflowx_events::decide(&view, sub, SequencedTime::from_event_clock(now), fresh)
                    .map(|d| {
                        let entries = d.record.map(|record| Entries {
                            record,
                            execution: Vec::new(),
                        });
                        (Outcome::Events(d.outcome), entries)
                    })
            }
            Submission::Execution(cmd) => {
                let draws = Draws {
                    run_ids: &mut std::iter::repeat_with(new_run_id),
                };
                let at = SequencedTime::from_event_clock(now);
                ironflowx_execution::decide(&view, cmd, by, at, draws).and_then(|d| {
                    let (outcome, execution, emits) = d.into_parts();
                    let events = ironflowx_events::sequence(&view, emits, fresh)?;
                    let some = !execution.is_empty() || !events.is_empty();
                    let entries = some.then_some(Entries {
                        record: Record { time: now, events },
                        execution: execution.into_iter().map(Into::into).collect(),
                    });
                    Ok((Outcome::Execution(outcome), entries))
                })
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
        ironflowx_core::reach_fault_point(ironflowx_core::FaultPoint::BeforeAppend);
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

    fn fold(&self, offset: u64, id: uuid::Uuid, entries: &Entries, emitted_by: &Emitter) {
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
        match store::apply(&mut conn, offset, Some((id, entries, emitted_by)), false) {
            Ok(()) => self.progress.send_modify(|p| {
                p.applied = offset + 1;
                p.time = Some(entries.record.time);
            }),
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
    log: SegmentLog,
    conn: Connection,
    watermark: u64,
    last_time: Option<Timestamp>,
    revision: u64,
    directory_lock: DirectoryLock,
}

struct DirectoryLock(File);

impl Drop for DirectoryLock {
    fn drop(&mut self) {
        // A child forked before exec can still hold a descriptor for this lock.
        let _ = self.0.unlock();
    }
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

/// Whether `dir` holds a log with a segment, read without creating anything.
fn holds_segment(dir: &Path) -> std::io::Result<bool> {
    let entries = match std::fs::read_dir(dir.join("log")) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        entries => entries?,
    };
    for entry in entries {
        // The log's own rule: it ignores any other name, and creates a first segment beside it.
        let name = entry?.file_name();
        let base = name.to_str().and_then(|n| n.strip_suffix(".seg"));
        if base.is_some_and(|b| b.len() == 20 && b.bytes().all(|b| b.is_ascii_digit())) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Take the ledger lock before opening either store. `existing` refuses a log without records.
fn recover(dir: &Path, env: EnvRef, existing: bool) -> Result<Recovered, OpenError> {
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
    let directory_lock = DirectoryLock(directory_lock);
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
    if existing && log.watermark() == 0 {
        return Err(OpenError::Missing);
    }
    // The fold runs only after fsync, so a read store of this environment ahead of the log
    // proves the log lost durable records. Refuse, before a genesis could paper over it.
    if store::recorded(dir, env, log.watermark() == 0)?
        .is_some_and(|applied| applied > log.watermark())
    {
        return Err(OpenError::Corrupt);
    }
    if log.watermark() == 0 {
        #[cfg(feature = "__fault-points")]
        ironflowx_core::reach_fault_point(ironflowx_core::FaultPoint::BeforeGenesis);
        append(&log, ExpectedRevision::Empty, record::genesis(env))
            .map_err(|e| OpenError::Io(std::io::Error::other(format!("genesis: {e:?}"))))?;
        #[cfg(feature = "__fault-points")]
        ironflowx_core::reach_fault_point(ironflowx_core::FaultPoint::AfterGenesis);
    }
    match read_record(&log, 0)? {
        record::Decoded::Genesis { format, .. } if format != FORMAT => {
            return Err(OpenError::Format);
        }
        record::Decoded::Genesis { env: genesis, .. } if genesis != env => {
            return Err(OpenError::WrongIncarnation);
        }
        record::Decoded::Genesis { .. } => {}
        record::Decoded::Entries { .. } if existing => return Err(OpenError::Missing),
        record::Decoded::Entries { .. } => return Err(OpenError::Corrupt),
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
    // Every non-genesis record carries an ID the writer drew at the append, so the last applied
    // one identifies this prefix even across copies of the directory, whatever it holds (D117).
    let mut last_time = None;
    let expected = if from > 1 {
        let (id, entries, _) = read_entries(log, from - 1, env)?;
        last_time = Some(entries.record.time);
        // The fold stores the canonical text, the only form the log decodes.
        Some(id.to_string())
    } else {
        None
    };
    match store::last_record(&conn) {
        Ok(actual) if actual == expected => {}
        Ok(_) => return Ok(Err(std::io::Error::other("read store diverges from log"))),
        Err(e) => return Ok(Err(std::io::Error::other(e))),
    }
    while from < log.watermark() {
        let record = if from == 0 {
            None
        } else {
            let (id, entries, emitted_by) = read_entries(log, from, env)?;
            last_time = Some(entries.record.time);
            Some((id, entries, emitted_by))
        };
        if let Err(e) = store::apply(
            &mut conn,
            from,
            record.as_ref().map(|(id, r, by)| (*id, r, by)),
            true,
        ) {
            return Ok(Err(std::io::Error::other(e)));
        }
        from += 1;
    }
    Ok(Ok((conn, last_time)))
}

/// A later record: it must carry this incarnation's token and format (LED-8).
fn read_entries(
    log: &SegmentLog,
    offset: u64,
    env: EnvRef,
) -> Result<(uuid::Uuid, Entries, Emitter), OpenError> {
    match read_record(log, offset)? {
        record::Decoded::Entries {
            format,
            token,
            id,
            emitted_by,
            entries,
        } if format == FORMAT && token == env.token() => Ok((id, entries, emitted_by)),
        _ => Err(OpenError::Corrupt),
    }
}

fn read_record(log: &SegmentLog, offset: u64) -> Result<record::Decoded, OpenError> {
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

#[cfg(test)]
mod lock_tests {
    use super::*;

    #[test]
    fn dropping_the_ledger_unlocks_even_with_an_inherited_descriptor() {
        let dir = tempfile::tempdir().unwrap();
        let env = crate::new_env_ref();
        let open = || {
            Ledger::open_blocking(
                dir.path(),
                env,
                crate::ManualClock::new(Timestamp::from_millis(0)),
            )
        };
        let ledger = open().unwrap();
        // A fork before exec shares the file description, just as a duplicate does.
        let inherited = ledger._directory_lock.0.try_clone().unwrap();
        assert!(matches!(open(), Err(OpenError::Locked)));
        drop(ledger);
        open().unwrap();
        drop(inherited);
    }
}
