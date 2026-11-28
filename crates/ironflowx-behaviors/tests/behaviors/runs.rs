//! `functions.feature` and `execution.feature`, on the engine and, but for what is `@local`,
//! through the door. A scenario tagged `@pending` is not built yet: it runs apart, is reported,
//! and fails the run if it passes, so its tag is dropped when it can be.

use std::collections::BTreeSet;
use std::fmt;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cucumber::writer::Stats as _;
use cucumber::{World, given, then, when};
use ironflowx_cell::ManualClock;
use ironflowx_core::{Clock, ErrorKind, Lease, Scope, Secret, Timestamp};
use ironflowx_operations::Engine;
use ironflowx_proto::{
    BeginStepRequest, BeginStepResponse, Call, Claim, ClaimRunRequest, CompleteRunRequest,
    CompleteStepRequest, CompleteStepResponse, CreateKeyRequest, EmitRequest, GetFunctionRequest,
    GetRunRequest, HeartbeatRequest, HeartbeatResponse, ListFunctionsRequest, OpError, Operations,
    PutFunctionRequest, PutFunctionResponse, ReleaseRunRequest, Run, RunStatus, StartRunRequest,
    WhoamiRequest,
};
use tempfile::TempDir;

use super::adapter::{Adapter, SERVED, runs_remotely};

/// How many cases of each file pass on the engine, and through the door, an outline once per
/// example; and how many are pending, on the engine and through the door.
pub const FUNCTION_RUNS: (usize, usize) = (29, 29);
pub const FUNCTION_PENDING: (usize, usize) = (17, 17);
pub const EXECUTION_RUNS: (usize, usize) = (26, 19);
pub const EXECUTION_PENDING: (usize, usize) = (56, 49);

const T: i64 = 1_800_000_000_000;

#[derive(World)]
#[world(init = Self::new)]
pub struct Runs {
    /// Whether the engine is served through the door.
    remote: bool,
    /// The engine's clock, under either adapter, so a step can move it.
    clock: ManualClock,
    // Before `dir`: the engine must close before its directory goes.
    ops: Option<Adapter>,
    dir: TempDir,
    /// Key texts: the bootstrap key's, the builder's, the executor's, another executor's, and the
    /// key a FUN-008 row names.
    bootstrap: Option<String>,
    builder: Option<String>,
    executor: Option<String>,
    another: Option<String>,
    key: Option<String>,
    put: Option<Result<PutFunctionResponse, OpError>>,
    /// The first put of a scenario that puts twice.
    first_put: Option<PutFunctionResponse>,
    /// The names on each page of a listing, in order.
    pages: Vec<Vec<String>>,
    /// The scenario's other run, and the claims of a step that claims more than once.
    other_run: Option<String>,
    claims: Vec<Option<Claim>>,
    /// The run a scenario is about, and the lease of its claim.
    run_id: Option<String>,
    /// The runs a scenario started, in order.
    started: Vec<String>,
    /// How long the last claim took.
    took: Option<Duration>,
    lease: Option<String>,
    claim: Option<Result<Option<Claim>, OpError>>,
    /// The leases of a step that claims more than one run, in order.
    leases: Vec<String>,
    /// The run as it read before the call a step compares it after.
    before: Option<Run>,
    /// What the last begin, step completion or heartbeat answered.
    begun: Option<BeginStepResponse>,
    completed: Option<CompleteStepResponse>,
    beat: Option<HeartbeatResponse>,
    /// The runs of the last claims, in order.
    claimed: Vec<String>,
    /// A claim is to wait while the next step starts a run.
    waiting: bool,
    /// Where the engine's clock tells of each reading off the writer's thread, once armed.
    told: Arc<Mutex<Option<Sender<()>>>>,
    /// How the last call ended.
    outcome: Option<Result<(), OpError>>,
}

impl fmt::Debug for Runs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Runs")
            .field("put", &self.put)
            .field("claim", &self.claim)
            .field("outcome", &self.outcome)
            .finish_non_exhaustive()
    }
}

/// The scenario's clock, which tells `told` of each reading off the ledger writer's thread: an
/// admission's, or a claim's read's.
#[derive(Clone)]
struct Told<C> {
    clock: C,
    told: Arc<Mutex<Option<Sender<()>>>>,
}

impl<C> Told<C> {
    fn new(clock: C, told: &Arc<Mutex<Option<Sender<()>>>>) -> Self {
        Self {
            clock,
            told: told.clone(),
        }
    }
}

impl<C: Clock> Clock for Told<C> {
    fn now(&self) -> Timestamp {
        if std::thread::current().name() != Some("ironflowx-ledger-writer")
            && let Some(tx) = &*self.told.lock().unwrap()
        {
            let _ = tx.send(());
        }
        self.clock.now()
    }
}

fn call(key: &str) -> Call {
    Call::with_key(Secret::new(key.to_owned()))
}

fn scopes(text: &str) -> BTreeSet<Scope> {
    text.split(['"', ',', ' '])
        .filter(|s| !s.is_empty() && *s != "and")
        .map(|s| s.parse().expect("a scope"))
        .collect()
}

impl Runs {
    fn new() -> Self {
        Self {
            remote: false,
            clock: ManualClock::new(Timestamp::from_millis(T)),
            ops: None,
            dir: tempfile::tempdir().unwrap(),
            bootstrap: None,
            builder: None,
            executor: None,
            another: None,
            key: None,
            put: None,
            first_put: None,
            pages: Vec::new(),
            other_run: None,
            claims: Vec::new(),
            run_id: None,
            started: Vec::new(),
            took: None,
            lease: None,
            claim: None,
            leases: Vec::new(),
            before: None,
            begun: None,
            completed: None,
            beat: None,
            claimed: Vec::new(),
            waiting: false,
            told: Arc::default(),
            outcome: None,
        }
    }

    fn ops(&self) -> &Adapter {
        self.ops.as_ref().expect("a fresh engine")
    }

    async fn create(&self, scopes: BTreeSet<Scope>) -> String {
        let bootstrap = call(self.bootstrap.as_ref().expect("the bootstrap key"));
        let req = CreateKeyRequest {
            scopes,
            ..CreateKeyRequest::default()
        };
        let created = self.ops().create_key(&bootstrap, req).await.unwrap();
        created.key.expose().to_owned()
    }

    /// The builder's call, created with `read` and `write` if no step made one.
    async fn builder(&mut self) -> Call {
        if self.builder.is_none() {
            self.builder = Some(self.create(scopes("read write")).await);
        }
        call(self.builder.as_ref().unwrap())
    }

    fn executor(&self) -> Call {
        call(self.executor.as_ref().expect("an executor"))
    }

    async fn run(&self) -> Run {
        let bootstrap = call(self.bootstrap.as_ref().expect("the bootstrap key"));
        let run_id = self.run_id.clone().expect("a run");
        let req = GetRunRequest { run_id };
        self.ops().get_run(&bootstrap, req).await.unwrap()
    }

    async fn start(&mut self) {
        let builder = self.builder().await;
        let req = StartRunRequest {
            function: "charge".into(),
        };
        let started = self.ops().start_run(&builder, req).await.unwrap();
        assert!(!started.duplicate);
        self.run_id = Some(started.run_id.to_string());
        self.started.push(started.run_id.to_string());
    }

    async fn claim(&self, cx: &Call, functions: &[&str]) -> Result<Option<Claim>, OpError> {
        self.claim_for(cx, functions, None).await
    }

    /// A claim that asks for a lease of `lease_seconds`.
    async fn claim_for(
        &self,
        cx: &Call,
        functions: &[&str],
        lease_seconds: Option<u32>,
    ) -> Result<Option<Claim>, OpError> {
        let req = ClaimRunRequest {
            functions: functions.iter().map(|f| (*f).to_owned()).collect(),
            lease_seconds,
            wait_seconds: Some(0),
        };
        self.ops().claim_run(cx, req).await
    }

    /// The call of "another executor", made with a second `execute` key if the scenario has none.
    async fn another(&mut self) -> Call {
        if self.another.is_none() {
            self.another = Some(self.create(scopes("execute")).await);
        }
        call(self.another.as_ref().unwrap())
    }

    /// Another executor's claim of "charge", without waiting: the run as it read before, and the
    /// claim, for the steps that follow.
    async fn another_claims(&mut self) {
        let another = self.another().await;
        self.before = Some(self.run().await);
        self.claim = Some(self.claim(&another, &["charge@v1"]).await);
    }

    async fn begin(&self, cx: &Call, lease: &str, position: u32, name: &str) -> BeginStepResponse {
        let req = BeginStepRequest {
            lease: lease.to_owned(),
            position,
            name: name.to_owned(),
            ..BeginStepRequest::default()
        };
        self.ops().begin_step(cx, req).await.unwrap()
    }

    async fn release(&self, cx: &Call, lease: &str) -> Result<Timestamp, OpError> {
        let req = ReleaseRunRequest {
            lease: lease.to_owned(),
        };
        self.ops().release_run(cx, req).await
    }

    /// The executor claims the scenario's run: its lease.
    async fn claim_it(&mut self) {
        let claim = self.claim(&self.executor(), &["charge@v1"]).await.unwrap();
        let claim = claim.expect("a due run");
        assert_eq!(Some(claim.lease.run.to_string()), self.run_id);
        self.lease = Some(claim.lease.to_string());
    }

    async fn put_def(&mut self, req: PutFunctionRequest) -> Result<PutFunctionResponse, OpError> {
        let builder = self.builder().await;
        self.ops().put_function(&builder, req).await
    }

    async fn get(&mut self, name: &str) -> ironflowx_proto::Function {
        let builder = self.builder().await;
        let req = GetFunctionRequest { name: name.into() };
        self.ops().get_function(&builder, req).await.unwrap()
    }

    /// An executor's claim of `pairs`, made with a key of its own if the scenario has none.
    async fn executor_claims(&mut self, pairs: &[&str]) -> Option<Claim> {
        if self.executor.is_none() {
            self.executor = Some(self.create(scopes("execute")).await);
        }
        self.claim(&self.executor(), pairs).await.unwrap()
    }

    fn refused(&self, kind: ErrorKind) -> &OpError {
        match self.outcome.as_ref().expect("a call") {
            Err(e) if e.kind == kind => e,
            other => panic!("expected {kind:?}, got {other:?}"),
        }
    }
}

// ---- Background ----

#[given("a fresh engine")]
async fn fresh(w: &mut Runs) {
    if w.remote {
        let data = w.dir.path().join("data");
        let clock = Told::new(w.clock.clone(), &w.told);
        let (served, key) = Adapter::served_on(&data, clock).await;
        w.bootstrap = Some(key);
        w.ops = Some(served);
        return;
    }
    let clock = Told::new(w.clock.clone(), &w.told);
    let opened = Engine::open(w.dir.path(), clock, None).await.unwrap();
    w.bootstrap = Some(
        opened
            .minted
            .expect("a fresh engine mints")
            .expose()
            .to_owned(),
    );
    w.ops = Some(Adapter::Engine(opened.engine));
}

#[given(regex = r"^a builder with scopes (.+)$")]
async fn builder(w: &mut Runs, held: String) {
    w.builder = Some(w.create(scopes(&held)).await);
}

#[given(regex = r#"^an executor with scope "execute"$"#)]
async fn executor(w: &mut Runs) {
    w.executor = Some(w.create(scopes("execute")).await);
}

#[given(regex = r#"^function "charge" with code "v1", no trigger and no policy$"#)]
async fn charge(w: &mut Runs) {
    let builder = w.builder().await;
    let req = PutFunctionRequest {
        name: "charge".into(),
        code: "v1".into(),
        ..PutFunctionRequest::default()
    };
    w.ops().put_function(&builder, req).await.unwrap();
}

// ---- Definitions ----

#[when(
    regex = r#"^the builder puts function "charge" with code "v1", trigger "order.placed" and 5 attempts$"#
)]
async fn put_charge(w: &mut Runs) {
    let builder = w.builder().await;
    let req = PutFunctionRequest {
        name: "charge".into(),
        code: "v1".into(),
        triggers: vec!["order.placed".into()],
        max_attempts: Some(5),
        ..PutFunctionRequest::default()
    };
    w.put = Some(w.ops().put_function(&builder, req).await);
}

/// As the step above, and it took: "charge" reads back as that definition.
#[given(
    regex = r#"^the builder put function "charge" with code "v1", trigger "order.placed" and 5 attempts$"#
)]
async fn charge_was_put(w: &mut Runs) {
    put_charge(w).await;
    assert!(w.put.take().expect("a put").unwrap().changed);
    let f = w.get("charge").await;
    assert_eq!(
        (f.code.as_str(), f.triggers.as_slice(), f.max_attempts),
        ("v1", ["order.placed".to_owned()].as_slice(), Some(5))
    );
}

#[then("the put reports a change")]
fn put_changed(w: &mut Runs) {
    let put = w.put.as_ref().expect("a put").as_ref().unwrap();
    assert!(put.changed);
}

#[then(
    regex = r#"^getting "charge" shows code "v1", trigger "order.placed", 5 attempts, no delays, and the builder's key as who put it$"#
)]
async fn got_charge(w: &mut Runs) {
    let builder = w.builder().await;
    let req = GetFunctionRequest {
        name: "charge".into(),
    };
    let f = w.ops().get_function(&builder, req).await.unwrap();
    let me = w.ops().whoami(&builder, WhoamiRequest {}).await.unwrap();
    assert_eq!((f.name.as_str(), f.code.as_str()), ("charge", "v1"));
    assert_eq!(f.triggers, ["order.placed"]);
    assert_eq!(f.max_attempts, Some(5));
    assert_eq!((f.first_delay_seconds, f.max_delay_seconds), (None, None));
    assert_eq!(
        f.put_by.to_string(),
        format!("key:{}", me.key_id.expect("a key's call"))
    );
    // What the put answered is what reads back.
    assert_eq!(
        Some(&f),
        w.put.as_ref().unwrap().as_ref().ok().map(|p| &p.function)
    );
}

#[given(regex = r#"^the builder put function "charge" with code "v1" and trigger "order.placed"$"#)]
async fn put_charge_once(w: &mut Runs) {
    let req = PutFunctionRequest {
        name: "charge".into(),
        code: "v1".into(),
        triggers: vec!["order.placed".into()],
        ..PutFunctionRequest::default()
    };
    w.first_put = Some(w.put_def(req).await.unwrap());
}

#[when("the builder puts the same definition again")]
async fn put_again(w: &mut Runs) {
    // A put that appended would be stamped later than the first.
    w.clock.step(1_000);
    let first = &w.first_put.as_ref().expect("a first put").function;
    let req = PutFunctionRequest {
        name: first.name.clone(),
        code: first.code.clone(),
        triggers: first.triggers.clone(),
        ..PutFunctionRequest::default()
    };
    w.put = Some(w.put_def(req).await);
}

#[then("the put reports no change")]
fn put_unchanged(w: &mut Runs) {
    let put = w.put.as_ref().expect("a put").as_ref().unwrap();
    assert!(!put.changed);
    assert_eq!(
        Some(&put.function),
        w.first_put.as_ref().map(|p| &p.function)
    );
}

#[then(r#"getting "charge" shows the first put's time"#)]
async fn first_time(w: &mut Runs) {
    let f = w.get("charge").await;
    let first = w.first_put.as_ref().unwrap().function.put_at;
    assert_eq!(f.put_at, first);
}

#[when(regex = r#"^the builder puts function "charge" with code "v2" and no trigger$"#)]
async fn put_v2(w: &mut Runs) {
    let req = PutFunctionRequest {
        name: "charge".into(),
        code: "v2".into(),
        ..PutFunctionRequest::default()
    };
    w.put = Some(w.put_def(req).await);
}

#[when(regex = r#"^a client emits "([^"]+)"$"#)]
async fn emits(w: &mut Runs, name: String) {
    let builder = w.builder().await;
    let req = EmitRequest {
        name,
        data: None,
        metadata: None,
        version: None,
        idempotency_key: None,
    };
    w.ops().emit(&builder, req).await.unwrap();
}

#[then(regex = r#"^getting "charge" shows code "v2", no trigger and no number of attempts$"#)]
async fn got_v2(w: &mut Runs) {
    let f = w.get("charge").await;
    assert_eq!(f.code, "v2");
    assert_eq!(f.triggers, Vec::<String>::new());
    assert_eq!(
        (f.max_attempts, f.first_delay_seconds, f.max_delay_seconds),
        (None, None, None)
    );
}

/// No run of either label is due. It cannot fail before events create runs (#221).
#[then(regex = r#"^"charge" has no run$"#)]
async fn no_run(w: &mut Runs) {
    assert_eq!(w.executor_claims(&["charge@v1", "charge@v2"]).await, None);
}

#[when(regex = r"^the builder puts a function with (.+)$")]
async fn put_with(w: &mut Runs, what: String) {
    let base = PutFunctionRequest {
        name: "charge".into(),
        code: "v1".into(),
        ..PutFunctionRequest::default()
    };
    let trigger = what
        .strip_prefix("the trigger \"")
        .and_then(|t| t.strip_suffix('"'))
        .filter(|t| !t.contains('"'));
    let req = match what.as_str() {
        "an empty name" => PutFunctionRequest {
            name: String::new(),
            ..base
        },
        "a name of 129 bytes" => PutFunctionRequest {
            name: "x".repeat(129),
            ..base
        },
        r#"the name "a@b""# => PutFunctionRequest {
            name: "a@b".into(),
            ..base
        },
        "an empty code label" => PutFunctionRequest {
            code: String::new(),
            ..base
        },
        "11 triggers" => PutFunctionRequest {
            triggers: (0..11).map(|i| format!("order.e{i}")).collect(),
            ..base
        },
        r#"the trigger "order.placed" twice"# => PutFunctionRequest {
            triggers: vec!["order.placed".into(), "order.placed".into()],
            ..base
        },
        "0 attempts" => PutFunctionRequest {
            max_attempts: Some(0),
            ..base
        },
        "101 attempts" => PutFunctionRequest {
            max_attempts: Some(101),
            ..base
        },
        "a first delay above its longest delay" => PutFunctionRequest {
            first_delay_seconds: Some(61),
            max_delay_seconds: Some(60),
            ..base
        },
        other => match trigger {
            Some(t) => PutFunctionRequest {
                triggers: vec![t.into()],
                ..base
            },
            None => panic!("no such definition: {other}"),
        },
    };
    w.outcome = Some(w.put_def(req).await.map(drop));
}

#[then("it is refused as invalid")]
fn invalid(w: &mut Runs) {
    w.refused(ErrorKind::Invalid);
}

#[given(regex = r#"^20 functions are triggered by "order.placed"$"#)]
async fn twenty(w: &mut Runs) {
    for i in 1..=20 {
        let req = PutFunctionRequest {
            name: format!("f{i:02}"),
            code: "v1".into(),
            triggers: vec!["order.placed".into()],
            ..PutFunctionRequest::default()
        };
        w.put_def(req).await.unwrap();
    }
}

#[when(regex = r#"^the builder puts another function triggered by "order.placed"$"#)]
async fn another(w: &mut Runs) {
    let req = PutFunctionRequest {
        name: "late".into(),
        code: "v1".into(),
        triggers: vec!["order.placed".into()],
        ..PutFunctionRequest::default()
    };
    w.outcome = Some(w.put_def(req).await.map(drop));
}

#[then(regex = r#"^it is refused as resource exhausted, with a hint that names "([^"]+)"$"#)]
fn exhausted(w: &mut Runs, event: String) {
    let e = w.refused(ErrorKind::ResourceExhausted);
    assert_eq!(e.message, "an event name triggers at most 20 functions");
    let hint =
        format!("{event:?} triggers 20 other functions already: put 1 of them without it first");
    assert_eq!(e.hint, Some(hint));
}

#[then("putting one of the 20 again with new code succeeds")]
async fn one_again(w: &mut Runs) {
    let req = PutFunctionRequest {
        name: "f07".into(),
        code: "v2".into(),
        triggers: vec!["order.placed".into()],
        ..PutFunctionRequest::default()
    };
    let put = w.put_def(req).await.unwrap();
    assert!(put.changed);
    assert_eq!(w.get("f07").await.code, "v2");
}

#[given(regex = r#"^functions "c", "a" and "b" were put$"#)]
async fn three(w: &mut Runs) {
    for name in ["c", "a", "b"] {
        let req = PutFunctionRequest {
            name: name.into(),
            code: "v1".into(),
            ..PutFunctionRequest::default()
        };
        w.put_def(req).await.unwrap();
    }
}

#[when(regex = r"^the builder lists functions 2 at a time$")]
async fn lists(w: &mut Runs) {
    let builder = w.builder().await;
    let mut cursor = None;
    loop {
        let req = ListFunctionsRequest {
            limit: std::num::NonZeroU32::new(2),
            cursor,
        };
        let page = w.ops().list_functions(&builder, req).await.unwrap();
        w.pages
            .push(page.functions.into_iter().map(|f| f.name).collect());
        cursor = page.next;
        if cursor.is_none() {
            break;
        }
        assert!(w.pages.len() < 10, "the pages end");
    }
}

#[then(regex = r#"^the pages are "a", "b" and then "c", with no page after it$"#)]
fn pages(w: &mut Runs) {
    assert_eq!(w.pages, [vec!["a", "b"], vec!["c"]]);
}

#[given(regex = r#"^a key with every scope but "(\w+)"$"#)]
async fn every_scope_but(w: &mut Runs, but: String) {
    let but: Scope = but.parse().unwrap();
    let all = [Scope::Read, Scope::Write, Scope::Admin, Scope::Execute];
    let held = all.into_iter().filter(|s| *s != but).collect();
    w.key = Some(w.create(held).await);
}

#[when(regex = r"^the key calls (.+)$")]
async fn key_calls(w: &mut Runs, operation: String) {
    let cx = call(w.key.as_ref().expect("a key"));
    let ops = w.ops();
    // Refused at admission, so what each request names does not matter.
    let lease = "0190b5f0-0000-7000-8000-000000000001.1".to_owned();
    let outcome = match operation.as_str() {
        "put a function" => {
            let req = PutFunctionRequest {
                name: "charge".into(),
                code: "v1".into(),
                ..PutFunctionRequest::default()
            };
            ops.put_function(&cx, req).await.map(drop)
        }
        "get a function" => {
            let req = GetFunctionRequest {
                name: "charge".into(),
            };
            ops.get_function(&cx, req).await.map(drop)
        }
        "list functions" => {
            let req = ListFunctionsRequest::default();
            ops.list_functions(&cx, req).await.map(drop)
        }
        "start a run" => {
            let req = StartRunRequest {
                function: "charge".into(),
            };
            ops.start_run(&cx, req).await.map(drop)
        }
        "get a run" => {
            let req = GetRunRequest {
                run_id: "0190b5f0-0000-7000-8000-000000000001".into(),
            };
            ops.get_run(&cx, req).await.map(drop)
        }
        "claim a run" => w.claim(&cx, &["charge@v1"]).await.map(drop),
        "begin a step" => {
            let req = BeginStepRequest {
                lease,
                position: 1,
                name: "pay".into(),
                ..BeginStepRequest::default()
            };
            ops.begin_step(&cx, req).await.map(drop)
        }
        "complete a step" => {
            let req = CompleteStepRequest {
                lease,
                position: 1,
                output: None,
            };
            ops.complete_step(&cx, req).await.map(drop)
        }
        "complete a run" => {
            let req = CompleteRunRequest {
                lease,
                steps: 0,
                output: None,
            };
            ops.complete_run(&cx, req).await.map(drop)
        }
        "heartbeat" => {
            let req = HeartbeatRequest {
                leases: vec![lease],
            };
            ops.heartbeat(&cx, req).await.map(drop)
        }
        "release a run" => {
            let req = ReleaseRunRequest { lease };
            ops.release_run(&cx, req).await.map(drop)
        }
        other => panic!("no such call: {other}"),
    };
    w.outcome = Some(outcome);
}

#[then(regex = r#"^it is refused as permission denied, with a hint that names "(\w+)"$"#)]
fn permission_denied(w: &mut Runs, scope: String) {
    let e = w.refused(ErrorKind::PermissionDenied);
    assert_eq!(e.message, format!("missing scope: {scope}"));
    let hint = e.hint.as_deref().unwrap_or_default();
    assert!(hint.contains(&format!("scope {scope}")), "{hint}");
}

// ---- Claiming ----

#[given(regex = r#"^a run of "charge" was started$"#)]
async fn started(w: &mut Runs) {
    w.start().await;
}

#[given("it reads as queued, due at once, created by the key that started it")]
async fn reads_queued(w: &mut Runs) {
    let run = w.run().await;
    let builder = w.builder().await;
    let me = w.ops().whoami(&builder, WhoamiRequest {}).await.unwrap();
    assert_eq!(run.status, RunStatus::Queued);
    assert_eq!(run.due_at, Some(run.created_at), "due at once");
    assert_eq!(
        run.created_by.to_string(),
        format!("key:{}", me.key_id.expect("a key's call"))
    );
    assert_eq!((run.execution, run.lease_until), (0, None));
    assert_eq!(run.input.as_deref(), Some(b"null".as_slice()));
}

#[when(regex = r"^the executor claims it and completes it with output (.+)$")]
async fn claims_and_completes(w: &mut Runs, output: String) {
    w.claim_it().await;
    let req = CompleteRunRequest {
        lease: w.lease.clone().unwrap(),
        steps: 0,
        output: Some(output.into_bytes()),
    };
    let end = w.ops().complete_run(&w.executor(), req).await.unwrap();
    assert_eq!(Some(end.run_id.to_string()), w.run_id);
    assert_eq!(end.status, RunStatus::Completed);
}

#[then(regex = r"^the run reads as completed with output (.+), 0 steps and execution 1$")]
async fn reads_completed(w: &mut Runs, output: String) {
    let run = w.run().await;
    assert_eq!(run.status, RunStatus::Completed);
    assert_eq!(run.output, Some(output.into_bytes()), "byte for byte");
    assert_eq!((run.steps, run.execution), (0, 1));
    assert!(run.ended_at.is_some_and(|t| t >= run.created_at));
    assert_eq!((run.lease_until, run.due_at), (None, None));
}

#[given(regex = r#"^the builder put "charge" with code "v2" and started another run$"#)]
async fn v2_and_another(w: &mut Runs) {
    let req = PutFunctionRequest {
        name: "charge".into(),
        code: "v2".into(),
        ..PutFunctionRequest::default()
    };
    assert!(w.put_def(req).await.unwrap().changed);
    let builder = w.builder().await;
    let req = StartRunRequest {
        function: "charge".into(),
    };
    let started = w.ops().start_run(&builder, req).await.unwrap();
    w.other_run = Some(started.run_id.to_string());
}

#[when(regex = r#"^an executor of "charge@v2" claims without waiting, twice$"#)]
async fn claims_v2_twice(w: &mut Runs) {
    for _ in 0..2 {
        let claim = w.executor_claims(&["charge@v2"]).await;
        w.claims.push(claim);
    }
}

#[then("the first claim is the second run, and the second claim is empty")]
fn first_is_second(w: &mut Runs) {
    let [first, second] = w.claims.as_slice() else {
        panic!("two claims: {:?}", w.claims);
    };
    let first = first.as_ref().expect("a claim");
    assert_eq!(Some(first.lease.run.to_string()), w.other_run);
    assert_eq!(
        (first.function.as_str(), first.code.as_str()),
        ("charge", "v2")
    );
    assert_eq!(second, &None);
}

#[then(regex = r#"^an executor of "charge@v1" claims the first run$"#)]
async fn v1_claims_first(w: &mut Runs) {
    let claim = w.executor_claims(&["charge@v1"]).await.expect("a claim");
    assert_eq!(Some(claim.lease.run.to_string()), w.run_id);
    assert_eq!(claim.code, "v1");
}

#[when(regex = r#"^the executor claims "([^"]+)" without waiting$"#)]
async fn claims_pair(w: &mut Runs, pair: String) {
    w.before = Some(w.run().await);
    w.claim = Some(w.claim(&w.executor(), &[&pair]).await);
}

#[then("its claim is empty")]
async fn claim_empty(w: &mut Runs) {
    let claim = w.claim.as_ref().expect("a claim");
    assert_eq!(claim, &Ok(None));
    // Nothing changed, unless the claim failed the run (EXE-035, whose next step reads it).
    if let Some(before) = w.before.as_ref() {
        assert_eq!(&w.run().await, before);
    }
    if let Some(took) = w.took {
        // It waited what it asked for, and no longer.
        let asked = Duration::from_secs(1);
        assert!(asked <= took && took < asked * 2, "{took:?}");
    }
}

#[given(regex = r#"^a run of "charge" was started and the executor claimed it$"#)]
async fn started_and_claimed(w: &mut Runs) {
    w.start().await;
    w.claim_it().await;
}

#[when("another executor claims without waiting")]
async fn another_claims(w: &mut Runs) {
    w.another_claims().await;
}

#[then("the run reads as running with a lease deadline")]
async fn running_with_deadline(w: &mut Runs) {
    let run = w.run().await;
    assert_eq!((run.status, run.execution), (RunStatus::Running, 1));
    // Claimed at the clock's reading, for the default 60 seconds.
    let deadline = Timestamp::from_millis(w.clock.now().as_millis() + 60_000);
    assert_eq!(run.lease_until, Some(deadline));
}

// ---- Steps and replay ----

#[given(
    regex = r#"^an execution completed step 1 "reserve" with output (.+) and then released its run$"#
)]
async fn completed_and_released(w: &mut Runs, output: String) {
    w.start().await;
    w.claim_it().await;
    let (cx, lease) = (w.executor(), w.lease.clone().unwrap());
    let begun = w.begin(&cx, &lease, 1, "reserve").await;
    assert_eq!(begun, BeginStepResponse::Run { attempt: 1 });
    let req = CompleteStepRequest {
        lease: lease.clone(),
        position: 1,
        output: Some(output.into_bytes()),
    };
    w.ops().complete_step(&cx, req).await.unwrap();
    w.release(&cx, &lease).await.unwrap();
}

#[when(regex = r#"^another execution begins step 1 "reserve"$"#)]
async fn another_begins(w: &mut Runs) {
    let another = w.another().await;
    let claim = w.claim(&another, &["charge@v1"]).await.unwrap();
    let claim = claim.expect("the released run is due");
    assert_eq!(claim.lease.execution, 2);
    let lease = claim.lease.to_string();
    w.begun = Some(w.begin(&another, &lease, 1, "reserve").await);
    w.lease = Some(lease);
}

#[then(regex = r"^the engine answers completed, with (.+) byte for byte$")]
fn answers_recorded(w: &mut Runs, output: String) {
    let output = output.into_bytes();
    let begun = w.begun.as_ref().expect("a begin");
    assert_eq!(begun, &BeginStepResponse::Completed { output });
}

#[then(regex = r#"^beginning step 2 "pay" answers run$"#)]
async fn step_2_runs(w: &mut Runs) {
    let (cx, lease) = (w.another().await, w.lease.clone().unwrap());
    let begun = w.begin(&cx, &lease, 2, "pay").await;
    assert_eq!(begun, BeginStepResponse::Run { attempt: 1 });
}

#[given(regex = r#"^runs 1, 2 and 3 of "charge" were started in that order$"#)]
async fn three_started(w: &mut Runs) {
    for _ in 0..3 {
        w.start().await;
    }
}

#[when("the executor claims three times")]
async fn claims_three_times(w: &mut Runs) {
    let mut got = Vec::new();
    for _ in 0..3 {
        let claim = w.claim(&w.executor(), &["charge@v1"]).await.unwrap();
        got.push(claim.expect("a due run").lease.run.to_string());
    }
    w.claimed = got;
}

#[then("it gets runs 1, 2 and 3")]
fn got_in_order(w: &mut Runs) {
    assert_eq!(w.claimed, w.started);
}

#[given(regex = r#"^the executor is waiting on a claim of "charge"$"#)]
fn waiting(w: &mut Runs) {
    // The claim is sent with the start, in the next step, so that the two run at once.
    w.waiting = true;
}

#[when(regex = r#"^the builder starts "charge"$"#)]
async fn builder_starts(w: &mut Runs) {
    assert!(w.waiting, "a waiting claim");
    let builder = w.builder().await;
    let (ops, executor) = (w.ops(), w.executor());
    let req = ClaimRunRequest {
        functions: vec!["charge@v1".into()],
        ..ClaimRunRequest::default()
    };
    let (tx, readings) = std::sync::mpsc::channel();
    *w.told.lock().unwrap() = Some(tx);
    let sent = std::time::Instant::now();
    let (claim, run_id) = {
        let mut claim = std::pin::pin!(ops.claim_run(&executor, req));
        // Its admission and its read, through either adapter: it found nothing and waits.
        let mut read = 0;
        while read < 2 {
            tokio::select! {
                biased;
                early = &mut claim => panic!("the claim answered before a run existed: {early:?}"),
                () = tokio::time::sleep(Duration::from_millis(5)) => {}
            }
            read += readings.try_iter().count();
        }
        let req = StartRunRequest {
            function: "charge".into(),
        };
        let (claim, started) = tokio::join!(claim, ops.start_run(&builder, req));
        (claim, started.unwrap().run_id.to_string())
    };
    // It returned on the commit, not at a later 5 s reading.
    let took = sent.elapsed();
    assert!(took < Duration::from_secs(2), "{took:?}");
    w.run_id = Some(run_id);
    w.claim = Some(claim);
}

#[then("the claim returns that run")]
fn claim_returns_it(w: &mut Runs) {
    let claim = w.claim.as_ref().expect("a claim").as_ref().unwrap();
    let claim = claim.as_ref().expect("a run");
    assert_eq!(Some(claim.lease.run.to_string()), w.run_id);
    assert_eq!(claim.lease.execution, 1);
}

#[when(regex = r#"^the executor claims "charge" and waits 1 second$"#)]
async fn claims_waiting_a_second(w: &mut Runs) {
    let req = ClaimRunRequest {
        functions: vec!["charge@v1".into()],
        wait_seconds: Some(1),
        ..ClaimRunRequest::default()
    };
    let at = std::time::Instant::now();
    w.claim = Some(w.ops().claim_run(&w.executor(), req).await);
    w.took = Some(at.elapsed());
}

#[when(regex = r"^the executor claims with (.+)$")]
async fn claims_with(w: &mut Runs, what: String) {
    let pair = |n| format!("f{n}@v1");
    let mut req = ClaimRunRequest {
        functions: vec!["charge@v1".into()],
        wait_seconds: Some(0),
        ..ClaimRunRequest::default()
    };
    match what.as_str() {
        "no function" => req.functions.clear(),
        "101 functions" => req.functions = (0..101).map(pair).collect(),
        r#"the function "charge""# => req.functions = vec!["charge".into()],
        "a lease of 9 seconds" => req.lease_seconds = Some(9),
        "a lease of 901 seconds" => req.lease_seconds = Some(901),
        "a wait of 21 seconds" => req.wait_seconds = Some(21),
        other => panic!("no such claim: {other}"),
    }
    w.outcome = Some(w.ops().claim_run(&w.executor(), req).await.map(drop));
}

// ---- Leases and the fence ----

#[given("the executor claimed a run")]
async fn claimed_a_run(w: &mut Runs) {
    w.start().await;
    w.claim_it().await;
}

#[when("an executor with another key completes that run")]
async fn another_completes(w: &mut Runs) {
    w.another = Some(w.create(scopes("execute")).await);
    let cx = call(w.another.as_ref().unwrap());
    let req = CompleteRunRequest {
        lease: w.lease.clone().unwrap(),
        steps: 0,
        output: None,
    };
    w.outcome = Some(w.ops().complete_run(&cx, req).await.map(drop));
}

#[then("it is refused as a conflict")]
fn conflict(w: &mut Runs) {
    let e = w.refused(ErrorKind::Conflict);
    assert_eq!(e.hint.as_deref(), Some("another key holds this lease"));
    assert!(!e.kind.retryable());
}

#[then("the run reads as running")]
async fn reads_running(w: &mut Runs) {
    let run = w.run().await;
    assert_eq!(
        (run.status, run.execution, run.output),
        (RunStatus::Running, 1, None)
    );
}

#[given(regex = r#"^an execution of a 10 second lease began step 1 "pay"$"#)]
async fn ten_second_lease_began(w: &mut Runs) {
    w.start().await;
    let claim = w.claim_for(&w.executor(), &["charge@v1"], Some(10)).await;
    let lease = claim.unwrap().expect("a due run").lease.to_string();
    let begun = w.begin(&w.executor(), &lease, 1, "pay").await;
    assert_eq!(begun, BeginStepResponse::Run { attempt: 1 });
    w.lease = Some(lease);
}

#[given("its deadline passed and another executor claimed the run")]
async fn superseded(w: &mut Runs) {
    w.clock.step(10_000);
    let another = w.another().await;
    let claim = w.claim(&another, &["charge@v1"]).await.unwrap();
    assert_eq!(claim.expect("a lapsed lease is taken").lease.execution, 2);
    w.before = Some(w.run().await);
}

#[when(regex = r"^the first execution (.+)$")]
async fn first_execution(w: &mut Runs, call: String) {
    let (cx, lease) = (w.executor(), w.lease.clone().unwrap());
    let ops = w.ops();
    let outcome = match call.as_str() {
        "begins step 1" => {
            let req = BeginStepRequest {
                lease,
                position: 1,
                name: "pay".into(),
                ..BeginStepRequest::default()
            };
            ops.begin_step(&cx, req).await.map(drop)
        }
        "completes step 1" => {
            let req = CompleteStepRequest {
                lease,
                position: 1,
                output: None,
            };
            ops.complete_step(&cx, req).await.map(drop)
        }
        "completes the run" => {
            let req = CompleteRunRequest {
                lease,
                steps: 1,
                output: None,
            };
            ops.complete_run(&cx, req).await.map(drop)
        }
        "releases the run" => w.release(&cx, &lease).await.map(drop),
        other => panic!("no such call: {other}"),
    };
    w.outcome = Some(outcome);
}

#[then("it is refused as a conflict, with a hint to stop")]
fn conflict_stop(w: &mut Runs) {
    let e = w.refused(ErrorKind::Conflict);
    let stop = "this execution was superseded: stop working on this run";
    assert_eq!(e.hint.as_deref(), Some(stop));
}

#[then("the run is as it was")]
async fn as_it_was(w: &mut Runs) {
    assert_eq!(Some(w.run().await), w.before);
}

#[given("the executor claimed runs 1 and 2 with 10 second leases")]
async fn claimed_two(w: &mut Runs) {
    w.start().await;
    let first = w.run_id.clone();
    w.start().await;
    w.other_run = w.run_id.replace(first.unwrap());
    for _ in 0..2 {
        let claim = w.claim_for(&w.executor(), &["charge@v1"], Some(10)).await;
        w.leases
            .push(claim.unwrap().expect("a due run").lease.to_string());
    }
}

#[when("8 seconds pass, and it heartbeats run 1 only, and 4 seconds pass")]
async fn heartbeats_run_1(w: &mut Runs) {
    w.clock.step(8_000);
    let req = HeartbeatRequest {
        leases: vec![w.leases[0].clone()],
    };
    let beat = w.ops().heartbeat(&w.executor(), req).await.unwrap();
    let renewed = Timestamp::from_millis(w.clock.now().as_millis() + 10_000);
    assert_eq!(beat.results, [Ok(renewed)]);
    w.clock.step(4_000);
}

#[when("another executor claims without waiting, twice")]
async fn another_claims_twice(w: &mut Runs) {
    let another = w.another().await;
    for _ in 0..2 {
        let claim = w.claim(&another, &["charge@v1"]).await.unwrap();
        w.claims.push(claim);
    }
}

#[then("the first claim is run 2, and the second claim is empty")]
fn first_is_run_2(w: &mut Runs) {
    let [first, second] = w.claims.as_slice() else {
        panic!("two claims: {:?}", w.claims);
    };
    let first = first.as_ref().expect("a claim");
    assert_eq!(Some(first.lease.run.to_string()), w.other_run);
    assert_eq!(first.lease.execution, 2);
    assert_eq!(second, &None);
}

#[given("the executor claimed run 1 and released run 2")]
async fn claimed_one_released_two(w: &mut Runs) {
    claimed_two(w).await;
    let released = w.leases[1].clone();
    w.release(&w.executor(), &released).await.unwrap();
}

/// A second passes first, so a renewal moves the deadline.
#[when("it heartbeats both")]
async fn heartbeats_both(w: &mut Runs) {
    w.clock.step(1_000);
    let req = HeartbeatRequest {
        leases: w.leases.clone(),
    };
    w.beat = Some(w.ops().heartbeat(&w.executor(), req).await.unwrap());
}

#[then("run 1 is renewed and run 2 is a conflict")]
async fn renewed_and_conflict(w: &mut Runs) {
    let run = w.run().await;
    let beat = w.beat.as_ref().expect("a heartbeat");
    let [renewed, refused] = beat.results.as_slice() else {
        panic!("two results: {beat:?}");
    };
    // The Given claims with 10 second leases.
    let until = Timestamp::from_millis(w.clock.now().as_millis() + 10_000);
    assert_eq!((renewed, run.lease_until), (&Ok(until), Some(until)));
    let e = refused.as_ref().unwrap_err();
    let over = (ErrorKind::Conflict, Some("this execution is over"));
    assert_eq!((e.kind, e.hint.as_deref()), over);
}

#[given(regex = r#"^a run of "charge" lost 9 leases in a row with no step outcome$"#)]
async fn lost_nine(w: &mut Runs) {
    w.start().await;
    let claim = w.claim_for(&w.executor(), &["charge@v1"], Some(10)).await;
    claim.unwrap().expect("a due run");
    let another = w.another().await;
    for _ in 0..9 {
        w.clock.step(10_000);
        let claim = w.claim_for(&another, &["charge@v1"], Some(10)).await;
        claim.unwrap().expect("a lapsed lease is taken");
    }
    let run = w.run().await;
    assert_eq!((run.lost_leases, run.execution), (9, 10));
}

#[when("the tenth lapses and an executor claims without waiting")]
async fn tenth_lapses(w: &mut Runs) {
    w.clock.step(10_000);
    w.before = None;
    w.claim = Some(w.claim(&w.executor(), &["charge@v1"]).await);
}

#[then(r#"the run reads as failed with "ironflowx.executor_lost""#)]
async fn failed_executor_lost(w: &mut Runs) {
    let run = w.run().await;
    let code = run.error.map(|e| e.code);
    assert_eq!(
        (run.status, code.as_deref()),
        (RunStatus::Failed, Some("ironflowx.executor_lost"))
    );
    // Its count reads 10, its execution number is unchanged, and it ended at the claim.
    assert_eq!(
        (run.lost_leases, run.execution, run.lease_until),
        (10, 10, None)
    );
    assert_eq!(run.ended_at, Some(w.clock.now()));
}

#[when("30 seconds pass and it completes step 1 with output 1")]
async fn late_completion(w: &mut Runs) {
    w.clock.step(30_000);
    let req = CompleteStepRequest {
        lease: w.lease.clone().unwrap(),
        position: 1,
        output: Some(b"1".to_vec()),
    };
    w.completed = Some(w.ops().complete_step(&w.executor(), req).await.unwrap());
}

#[then("the engine answers completed")]
fn answers_completed(w: &mut Runs) {
    let output = b"1".to_vec();
    let completed = w.completed.as_ref().expect("a completion");
    assert_eq!(completed, &CompleteStepResponse::Completed { output });
}

#[then("its lease has a new deadline")]
async fn new_deadline(w: &mut Runs) {
    let run = w.run().await;
    let renewed = Timestamp::from_millis(w.clock.now().as_millis() + 10_000);
    assert_eq!(
        (run.status, run.lease_until),
        (RunStatus::Running, Some(renewed))
    );
}

#[given("the executor claimed a run and released it")]
async fn claimed_and_released(w: &mut Runs) {
    w.start().await;
    w.claim_it().await;
    let lease = w.lease.clone().unwrap();
    w.release(&w.executor(), &lease).await.unwrap();
}

#[then("it gets that run, execution 2")]
fn gets_it(w: &mut Runs) {
    let claim = w.claim.as_ref().expect("a claim").as_ref().unwrap();
    let claim = claim.as_ref().expect("the released run is due at once");
    assert_eq!(Some(claim.lease.run.to_string()), w.run_id);
    assert_eq!(claim.lease.execution, 2);
}

/// "Failed no attempt" reads that no error is recorded; it cannot fail until failures are (#225).
#[then("the run reads as having lost no lease and failed no attempt")]
async fn nothing_counted(w: &mut Runs) {
    let run = w.run().await;
    assert_eq!((run.lost_leases, run.error), (0, None));
}

#[when(regex = r"^the executor completes the run of lease (.+)$")]
async fn completes_lease(w: &mut Runs, lease: String) {
    let lease = match lease.as_str() {
        "built: an unknown run, execution 1" => {
            let run = "0190b5f0-0000-7000-8000-000000000999".parse().unwrap();
            Lease { run, execution: 1 }.to_string()
        }
        quoted => serde_json::from_str(quoted).expect("a quoted lease"),
    };
    let req = CompleteRunRequest {
        lease,
        steps: 0,
        output: None,
    };
    w.outcome = Some(w.ops().complete_run(&w.executor(), req).await.map(drop));
}

#[then("it is refused as not found")]
fn not_found(w: &mut Runs) {
    let e = w.refused(ErrorKind::NotFound);
    assert_eq!(e.message, "run not found");
}

/// Runs `file` through the door or on the engine, its pending cases apart: whether a case failed,
/// how many passed, and how many pending cases ran without passing.
pub async fn run(file: &'static str, remote: bool) -> (bool, usize, usize) {
    let pass = async |pending: bool| {
        let cucumber = Runs::cucumber::<&str>()
            .with_parser(super::Unescaped)
            .before(move |_, _, _, world| {
                world.remote = remote;
                Box::pin(async {})
            })
            .after(move |_, _, _, _, world| {
                Box::pin(async move {
                    if let Some(ops) = world.and_then(|w| w.ops.take()) {
                        assert_eq!(matches!(ops, Adapter::Door(..)), remote, "the adapter");
                        ops.close().await;
                    }
                })
            })
            .max_concurrent_scenarios(if remote { SERVED } else { 64 });
        let filter = move |_: &_, _: Option<&_>, scenario: &cucumber::gherkin::Scenario| {
            (!remote || runs_remotely(scenario))
                && scenario.tags.iter().any(|t| t == "pending") == pending
        };
        // A pending case is not built: a step it needs may be missing, so it is not failed for
        // that; it must not pass. Each pass: whether it failed, and its passed, skipped and failed
        // cases.
        let counts = |failed: bool, stats: &cucumber::writer::summarize::Stats| {
            (failed, stats.passed, stats.skipped, stats.failed)
        };
        if pending {
            let w = cucumber.filter_run(file, filter).await;
            counts(w.execution_has_failed(), w.scenarios_stats())
        } else {
            let w = cucumber.fail_on_skipped().filter_run(file, filter).await;
            counts(w.execution_has_failed(), w.scenarios_stats())
        }
    };
    let (failed, passed, ..) = pass(false).await;
    let (_, pending_passed, skipped, not_passed) = pass(true).await;
    assert_eq!(
        pending_passed, 0,
        "{file}: a pending case passes: drop its @pending tag"
    );
    (failed, passed, skipped + not_passed)
}
