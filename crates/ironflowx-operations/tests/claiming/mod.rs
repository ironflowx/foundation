//! What the claim tests share: a clock that stops and counts its readings, and an engine with
//! function "charge" and an executor.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ironflowx_core::{Clock, ErrorKind, Scope, Secret, Timestamp};
use ironflowx_operations::Engine;
use ironflowx_proto::{
    Call, Claim, ClaimRunRequest, CreateKeyRequest, GetFunctionRequest, GetRunRequest,
    ListEnvironmentsRequest, OpError, Operations, PutFunctionRequest, RunStatus, StartRunRequest,
};

pub const T: i64 = 1_800_000_000_000;
const WRITER: &str = "ironflowx-ledger-writer";

/// Where a thread stops: it says it is there, then waits to be let go.
pub struct Stop(Sender<()>, Receiver<()>);

impl Stop {
    pub fn new() -> (Self, Receiver<()>, Sender<()>) {
        let ((tell, reached), (go, gone)) = (channel(), channel());
        (Self(tell, gone), reached, go)
    }

    pub fn reach(self) {
        self.0.send(()).unwrap();
        // The test may have gone: then there is nothing to wait for.
        let _ = self.1.recv();
    }
}

/// A clock a test sets, which counts the writer's readings and stops one reading when armed.
#[derive(Clone, Default)]
pub struct Probe(Arc<Probed>);

#[derive(Default)]
pub struct Probed {
    ms: AtomicI64,
    writer_readings: AtomicUsize,
    /// The writer's readings: how many more pass before one stops, and the stop.
    writer_stop: Mutex<Option<(usize, Stop)>>,
    /// Readings off the writer's thread: how many more pass before one stops, and the stop.
    other_stop: Mutex<Option<(usize, Stop)>>,
    /// Each reading off the writer's thread is told here.
    others: Mutex<Option<Sender<()>>>,
    panics: AtomicBool,
}

impl Probe {
    pub fn at(ms: i64) -> Self {
        let probe = Self::default();
        probe.set(ms);
        probe
    }

    pub fn set(&self, ms: i64) {
        self.0.ms.store(ms, Ordering::SeqCst);
    }

    pub fn writer_readings(&self) -> usize {
        self.0.writer_readings.load(Ordering::SeqCst)
    }

    /// Stops the writer's reading after `pass` more: a submission held before its decision.
    pub fn stop_writer(&self, pass: usize) -> (Receiver<()>, Sender<()>) {
        let (stop, reached, go) = Stop::new();
        *self.0.writer_stop.lock().unwrap() = Some((pass, stop));
        (reached, go)
    }

    /// Stops the reading off the writer's thread after `pass` more.
    pub fn stop_other(&self, pass: usize) -> (Receiver<()>, Sender<()>) {
        let (stop, reached, go) = Stop::new();
        *self.0.other_stop.lock().unwrap() = Some((pass, stop));
        (reached, go)
    }

    /// Tells the receiver of each reading off the writer's thread from now on.
    pub fn tell_others(&self) -> Receiver<()> {
        let (tx, rx) = channel();
        *self.0.others.lock().unwrap() = Some(tx);
        rx
    }
}

impl Clock for Probe {
    fn now(&self) -> Timestamp {
        let at = Timestamp::from_millis(self.0.ms.load(Ordering::SeqCst));
        if std::thread::current().name() == Some(WRITER) {
            self.0.writer_readings.fetch_add(1, Ordering::SeqCst);
            assert!(
                !self.0.panics.load(Ordering::SeqCst),
                "the writer's clock fails"
            );
            if let Some(stop) = countdown(&self.0.writer_stop) {
                stop.reach();
            }
            return at;
        }
        if let Some(tx) = &*self.0.others.lock().unwrap() {
            let _ = tx.send(());
        }
        if let Some(stop) = countdown(&self.0.other_stop) {
            stop.reach();
        }
        at
    }
}

/// The stop, once no reading is left to pass.
fn countdown(armed: &Mutex<Option<(usize, Stop)>>) -> Option<Stop> {
    let mut armed = armed.lock().unwrap();
    match armed.take() {
        Some((0, stop)) => Some(stop),
        Some((n, stop)) => {
            *armed = Some((n - 1, stop));
            None
        }
        None => None,
    }
}

pub struct Fixture {
    pub engine: Arc<Engine>,
    pub admin: Call,
    /// The executor's key text.
    executor: String,
    pub executor_id: String,
    pub dir: tempfile::TempDir,
}

/// An engine with function "charge" at code "v1", and an executor whose key expires an hour after
/// the clock's reading now.
pub async fn fixture(clock: impl Clock + Clone + Send + Sync + 'static) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let expires = clock.now().as_millis() + 3_600_000;
    let opened = Engine::open(dir.path(), clock, None).await.unwrap();
    let (engine, admin) = (opened.engine, Call::with_key(opened.minted.unwrap()));
    let req = CreateKeyRequest {
        scopes: BTreeSet::from([Scope::Execute]),
        expires_at: Some(Timestamp::from_millis(expires)),
        ..CreateKeyRequest::default()
    };
    let created = engine.create_key(&admin, req).await.unwrap();
    let put = PutFunctionRequest {
        name: "charge".into(),
        code: "v1".into(),
        ..PutFunctionRequest::default()
    };
    engine.put_function(&admin, put).await.unwrap();
    // A read waits for the put's fold, so no fold wakes a claim the test sends next.
    let get = GetFunctionRequest {
        name: "charge".into(),
    };
    engine.get_function(&admin, get).await.unwrap();
    Fixture {
        engine: Arc::new(engine),
        admin,
        executor: created.key.expose().to_owned(),
        executor_id: created.info.key_id.to_string(),
        dir,
    }
}

pub fn claim(wait: Option<u32>) -> ClaimRunRequest {
    ClaimRunRequest {
        functions: vec!["charge@v1".into()],
        wait_seconds: wait,
        ..ClaimRunRequest::default()
    }
}

pub type Claimed = Result<Option<Claim>, OpError>;

impl Fixture {
    pub fn executor(&self) -> Call {
        Call::with_key(Secret::new(self.executor.clone()))
    }

    pub async fn start(&self) -> String {
        let req = StartRunRequest {
            function: "charge".into(),
        };
        let started = self.engine.start_run(&self.admin, req).await.unwrap();
        started.run_id.to_string()
    }

    pub async fn status(&self, run_id: &str) -> RunStatus {
        let req = GetRunRequest {
            run_id: run_id.to_owned(),
        };
        self.engine.get_run(&self.admin, req).await.unwrap().status
    }

    /// The executor's claim, sent on a task of its own: its answer, and when it came.
    pub fn spawn_claim(&self, wait: Option<u32>) -> tokio::task::JoinHandle<(Claimed, Instant)> {
        let (engine, cx) = (self.engine.clone(), self.executor());
        tokio::spawn(async move { (engine.claim_run(&cx, claim(wait)).await, Instant::now()) })
    }

    pub async fn ledger_dir(&self) -> PathBuf {
        let listed = self
            .engine
            .list_environments(&self.admin, ListEnvironmentsRequest {})
            .await;
        let env = listed.unwrap().remove(0).env;
        self.dir.path().join("ledgers").join(env.id().to_string())
    }

    /// Moves a run's due time in the read store, with no record: as a retry's would be (#225).
    pub async fn due_at(&self, run_id: &str, ms: i64) {
        // A read waits for the start's fold, which inserts the row.
        assert_eq!(self.status(run_id).await, RunStatus::Queued);
        let db = rusqlite::Connection::open(self.ledger_dir().await.join("read.db")).unwrap();
        let sql = "UPDATE runs SET due_at = ?1 WHERE id = ?2";
        assert_eq!(db.execute(sql, rusqlite::params![ms, run_id]).unwrap(), 1);
    }

    /// Fails the read store's next run insert: the ledger fails at the fold of a start.
    pub async fn fail_next_start(&self) {
        let db = rusqlite::Connection::open(self.ledger_dir().await.join("read.db")).unwrap();
        let sql = "CREATE TRIGGER t BEFORE INSERT ON runs BEGIN SELECT RAISE(ABORT, 'x'); END";
        db.execute_batch(sql).unwrap();
    }

    /// Closes the engine and opens it again on `clock`.
    pub async fn reopen(self, clock: impl Clock + Clone + Send + Sync + 'static) -> Self {
        let Self {
            engine,
            admin,
            executor,
            executor_id,
            dir,
        } = self;
        let engine = Arc::try_unwrap(engine).ok().expect("no claim in flight");
        engine.close().await;
        let opened = Engine::open(dir.path(), clock, None).await.unwrap();
        Self {
            engine: Arc::new(opened.engine),
            admin,
            executor,
            executor_id,
            dir,
        }
    }

    pub async fn close(self) {
        let engine = Arc::try_unwrap(self.engine)
            .ok()
            .expect("no claim in flight");
        engine.close().await;
    }
}

/// Every file of the ledger but the read store's, with its bytes: the log.
pub fn log(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else if !path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("read.db")
            {
                out.insert(path.clone(), std::fs::read(&path).unwrap());
            }
        }
    }
    out
}

/// Waits for `n` messages, at most 10 s.
pub fn receive(rx: &Receiver<()>, n: usize) {
    for _ in 0..n {
        rx.recv_timeout(Duration::from_secs(10)).unwrap();
    }
}

pub fn unavailable(claimed: &Claimed) -> bool {
    matches!(claimed, Err(e) if e.kind == ErrorKind::Unavailable)
}
