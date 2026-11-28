//! `functions.feature` and `execution.feature`, on the engine and, but for what is `@local`,
//! through the door. A scenario tagged `@pending` is not built yet: it runs apart, is reported,
//! and fails the run if it passes, so its tag is dropped when it can be.

use std::collections::BTreeSet;
use std::fmt;

use cucumber::writer::Stats as _;
use cucumber::{World, given, then, when};
use ironflowx_cell::ManualClock;
use ironflowx_core::{ErrorKind, Lease, Scope, Secret, Timestamp};
use ironflowx_operations::Engine;
use ironflowx_proto::{
    BeginStepRequest, Call, Claim, ClaimRunRequest, CompleteRunRequest, CompleteStepRequest,
    CreateKeyRequest, GetFunctionRequest, GetRunRequest, OpError, Operations, PutFunctionRequest,
    PutFunctionResponse, Run, RunStatus, StartRunRequest, WhoamiRequest,
};
use tempfile::TempDir;

use super::adapter::{Adapter, SERVED, runs_remotely};

/// How many cases of each file pass on the engine, and through the door, an outline once per
/// example; and how many are pending, on the engine and through the door.
pub const FUNCTION_RUNS: (usize, usize) = (9, 9);
pub const FUNCTION_PENDING: (usize, usize) = (37, 37);
pub const EXECUTION_RUNS: (usize, usize) = (5, 5);
pub const EXECUTION_PENDING: (usize, usize) = (77, 63);

const T: i64 = 1_800_000_000_000;

#[derive(World)]
#[world(init = Self::new)]
pub struct Runs {
    /// Whether the engine is served through the door.
    remote: bool,
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
    /// The run a scenario is about, and the lease of its claim.
    run_id: Option<String>,
    lease: Option<String>,
    claim: Option<Result<Option<Claim>, OpError>>,
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
            ops: None,
            dir: tempfile::tempdir().unwrap(),
            bootstrap: None,
            builder: None,
            executor: None,
            another: None,
            key: None,
            put: None,
            run_id: None,
            lease: None,
            claim: None,
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
    }

    async fn claim(&self, cx: &Call, functions: &[&str]) -> Result<Option<Claim>, OpError> {
        let req = ClaimRunRequest {
            functions: functions.iter().map(|f| (*f).to_owned()).collect(),
            wait_seconds: Some(0),
            ..ClaimRunRequest::default()
        };
        self.ops().claim_run(cx, req).await
    }

    /// The executor claims the scenario's run: its lease.
    async fn claim_it(&mut self) {
        let claim = self.claim(&self.executor(), &["charge@v1"]).await.unwrap();
        let claim = claim.expect("a due run");
        assert_eq!(Some(claim.lease.run.to_string()), self.run_id);
        self.lease = Some(claim.lease.to_string());
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
        let (served, key) = Adapter::served(&w.dir.path().join("data")).await;
        w.bootstrap = Some(key);
        w.ops = Some(served);
        return;
    }
    let clock = ManualClock::new(Timestamp::from_millis(T));
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

#[when(regex = r#"^the executor claims "([^"]+)" without waiting$"#)]
async fn claims_pair(w: &mut Runs, pair: String) {
    w.claim = Some(w.claim(&w.executor(), &[&pair]).await);
}

#[then("its claim is empty")]
async fn claim_empty(w: &mut Runs) {
    let claim = w.claim.as_ref().expect("a claim");
    assert_eq!(claim, &Ok(None));
    // Nothing was taken.
    assert_eq!(w.run().await.status, RunStatus::Queued);
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
