//! Execution rules against an in-memory read store: `decide`, then `fold` of what it decided.

use std::collections::BTreeMap;

use ironflowx_core::limits::MAX_DATA_BYTES;
use ironflowx_core::{Emitter, ErrorKind, Lease, RunId, SequencedTime, Timestamp};
use ironflowx_execution::{
    Begun, Command, Decision, Definition, Draws, Entry, Error, ExecutionTables, ExecutionView,
    Function, Outcome, Refusal, Run, RunEnd, RunError, Status, Step, claim_pairs, decide, decode,
    encode, fold,
};

const T: i64 = 1_800_000_000_000;

#[derive(Default)]
struct Store {
    functions: BTreeMap<String, Function>,
    runs: BTreeMap<String, Run>,
    steps: BTreeMap<(String, u32), Step>,
    time: i64,
    drawn: usize,
}

impl ExecutionView for Store {
    type Error = String;

    fn function(&self, name: &str) -> Result<Option<Function>, String> {
        Ok(self.functions.get(name).cloned())
    }

    fn triggered_by(&self, event: &str, other_than: &str) -> Result<u64, String> {
        let triggered = self.functions.values().filter(|f| {
            f.definition.name != other_than && f.definition.triggers.iter().any(|t| t == event)
        });
        Ok(triggered.count() as u64)
    }

    fn function_count(&self) -> Result<u64, String> {
        Ok(self.functions.len() as u64)
    }

    fn functions_after(&self, after: Option<&str>, limit: u32) -> Result<Vec<Function>, String> {
        let after = after.map_or(std::ops::Bound::Unbounded, |a| {
            std::ops::Bound::Excluded(a.to_owned())
        });
        let page = self.functions.range((after, std::ops::Bound::Unbounded));
        Ok(page.take(limit as usize).map(|(_, f)| f.clone()).collect())
    }

    fn run(&self, id: RunId) -> Result<Option<Run>, String> {
        Ok(self.runs.get(&id.to_string()).cloned())
    }

    fn step(&self, run: RunId, position: u32) -> Result<Step, String> {
        let step = self.steps.get(&(run.to_string(), position)).cloned();
        step.ok_or_else(|| format!("step {position} of {run} is lost"))
    }

    fn last_run_sequence(&self) -> Result<u64, String> {
        Ok(self.runs.values().map(|r| r.sequence).max().unwrap_or(0))
    }

    fn claimable(
        &self,
        pairs: &[(String, String)],
        now: Timestamp,
        skip: u32,
    ) -> Result<Option<Run>, String> {
        let due_at = |r: &Run| match r.status {
            Status::Queued => r.due_at,
            Status::Running => r.lease_until,
            Status::Completed | Status::Failed => None,
        };
        let mut due: Vec<_> = self
            .runs
            .values()
            .filter(|r| pairs.contains(&(r.function.clone(), r.code.clone())))
            .filter(|r| due_at(r).is_some_and(|d| d <= now))
            .collect();
        due.sort_by_key(|r| (due_at(r), r.sequence));
        Ok(due.into_iter().nth(skip as usize).cloned())
    }
}

impl ExecutionTables for Store {
    type Error = String;

    fn put_function(&mut self, f: &Function) -> Result<(), String> {
        self.functions.insert(f.definition.name.clone(), f.clone());
        Ok(())
    }

    fn create_run(&mut self, run: &Run) -> Result<(), String> {
        self.runs.insert(run.id.to_string(), run.clone());
        Ok(())
    }

    fn claim(
        &mut self,
        run: RunId,
        execution: u32,
        lease_until: Timestamp,
        lease_seconds: u32,
        by: &Emitter,
    ) -> Result<(), String> {
        let r = self.runs.get_mut(&run.to_string()).unwrap();
        r.status = Status::Running;
        r.execution = execution;
        r.lease_until = Some(lease_until);
        r.lease_seconds = lease_seconds;
        r.claimed_by = Some(by.clone());
        r.due_at = None;
        Ok(())
    }

    fn renew(&mut self, run: RunId, lease_until: Timestamp) -> Result<(), String> {
        let r = self.runs.get_mut(&run.to_string()).unwrap();
        assert_eq!(r.status, Status::Running, "a running run's lease");
        r.lease_until = Some(lease_until);
        Ok(())
    }

    fn lose_lease(&mut self, run: RunId, lost_leases: u32) -> Result<(), String> {
        let r = self.runs.get_mut(&run.to_string()).unwrap();
        assert_eq!(r.status, Status::Running, "a running run's lease");
        r.lost_leases = lost_leases;
        Ok(())
    }

    fn release(&mut self, run: RunId, at: Timestamp) -> Result<(), String> {
        let r = self.runs.get_mut(&run.to_string()).unwrap();
        assert_eq!(r.status, Status::Running, "a running run is released");
        r.status = Status::Queued;
        r.due_at = Some(at);
        r.lease_until = None;
        Ok(())
    }

    fn fail_run(&mut self, run: RunId, error: &RunError, at: Timestamp) -> Result<(), String> {
        let r = self.runs.get_mut(&run.to_string()).unwrap();
        assert_eq!(r.status, Status::Running, "a running run fails");
        r.status = Status::Failed;
        r.error = Some(error.clone());
        r.ended_at = Some(at);
        r.lease_until = None;
        Ok(())
    }

    fn begin_step(&mut self, run: RunId, step: &Step) -> Result<(), String> {
        let key = (run.to_string(), step.position);
        assert!(self.steps.insert(key, step.clone()).is_none(), "begun once");
        let r = self.runs.get_mut(&run.to_string()).unwrap();
        r.steps = step.position;
        r.step = Some(step.name.clone());
        Ok(())
    }

    fn complete_step(&mut self, run: RunId, position: u32, output: &[u8]) -> Result<(), String> {
        let step = self.steps.get_mut(&(run.to_string(), position)).unwrap();
        assert!(step.output.is_none(), "one outcome");
        step.output = Some(output.to_vec());
        let r = self.runs.get_mut(&run.to_string()).unwrap();
        r.step = None;
        r.lost_leases = 0;
        Ok(())
    }

    fn complete_run(&mut self, run: RunId, output: &[u8], at: Timestamp) -> Result<(), String> {
        let r = self.runs.get_mut(&run.to_string()).unwrap();
        r.status = Status::Completed;
        r.output = Some(output.to_vec());
        r.ended_at = Some(at);
        r.lease_until = None;
        Ok(())
    }
}

fn now(millis: i64) -> SequencedTime {
    // The ledger stamps event time; this stands in for it (D17).
    #[expect(clippy::disallowed_methods, reason = "a test stands in for the ledger")]
    SequencedTime::from_event_clock(Timestamp::from_millis(millis))
}

fn key(n: u8) -> Emitter {
    format!("key:0190b5f0-0000-7000-8000-00000000000{n}")
        .parse()
        .unwrap()
}

impl Store {
    /// Decides `cmd` as `by`, and folds what it decided: its outcome, entries and engine events.
    fn run_cmd(&mut self, cmd: Command, by: &Emitter) -> Result<Decision, Refusal> {
        let drawn = std::cell::Cell::new(self.drawn);
        let mut ids = std::iter::from_fn(|| {
            let i = drawn.replace(drawn.get() + 1);
            Some(
                format!("0190b5f0-0000-7000-8000-{:012}", i + 100)
                    .parse()
                    .unwrap(),
            )
        });
        let draws = Draws { run_ids: &mut ids };
        let decision = decide(&*self, &cmd, by, now(self.time), draws);
        self.drawn = drawn.get();
        let decision = decision.map_err(|e| match e {
            Error::Refused(r) => r,
            Error::Store(e) => panic!("the store failed: {e}"),
        })?;
        let (outcome, entries, events) = decision.clone().into_parts();
        let _ = (outcome, events);
        for entry in &entries {
            fold(
                self,
                &entry.clone().into(),
                Timestamp::from_millis(self.time),
                by,
            )
            .unwrap();
        }
        Ok(decision)
    }

    fn outcome(&mut self, cmd: Command, by: &Emitter) -> Result<Outcome, Refusal> {
        self.run_cmd(cmd, by).map(|d| d.into_parts().0)
    }
}

fn definition() -> Definition {
    Definition {
        name: "charge".into(),
        code: "v1".into(),
        triggers: Vec::new(),
        max_attempts: None,
        first_delay_seconds: None,
        max_delay_seconds: None,
    }
}

/// A store with "charge" put, a run of it started, and that run claimed by key 1: its lease.
fn claimed() -> (Store, Lease) {
    let mut s = Store {
        time: T,
        ..Store::default()
    };
    s.outcome(Command::PutFunction(definition()), &key(1))
        .unwrap();
    s.outcome(start(), &key(1)).unwrap();
    let Outcome::Claimed(Some(claim)) = s.outcome(claim(&["charge@v1"]), &key(1)).unwrap() else {
        panic!("a claim");
    };
    (s, claim.lease)
}

fn start() -> Command {
    Command::StartRun {
        function: "charge".into(),
    }
}

fn claim(pairs: &[&str]) -> Command {
    Command::Claim {
        functions: pairs.iter().map(|p| (*p).to_owned()).collect(),
        lease_seconds: None,
        wait_seconds: None,
    }
}

fn begin(lease: &str, position: u32, name: &str) -> Command {
    Command::BeginStep {
        lease: lease.to_owned(),
        position,
        name: name.to_owned(),
        once: false,
        input: None,
    }
}

fn complete_step(lease: &str, position: u32, output: &[u8]) -> Command {
    Command::CompleteStep {
        lease: lease.to_owned(),
        position,
        output: Some(output.to_vec()),
    }
}

fn complete(lease: &str, steps: u32) -> Command {
    Command::Complete {
        lease: lease.to_owned(),
        steps,
        output: None,
    }
}

fn kind_and_hint(r: &Refusal) -> (ErrorKind, Option<&str>) {
    (r.kind(), r.hint())
}

#[test]
fn a_run_is_put_started_claimed_stepped_and_completed() {
    let (mut s, lease) = claimed();
    let run = s.runs.values().next().unwrap().clone();
    assert_eq!((run.status, run.execution), (Status::Running, 1));
    assert_eq!(
        run.input, b"null",
        "a run's input is null until StartRun takes one"
    );
    assert_eq!(run.lease_until, Some(Timestamp::from_millis(T + 60_000)));
    assert_eq!(run.claimed_by, Some(key(1)));
    let lease = lease.to_string();
    let begun = s.outcome(begin(&lease, 1, "reserve"), &key(1)).unwrap();
    assert_eq!(begun, Outcome::StepBegun(Begun::Run { attempt: 1 }));
    let output = br#"{"id":  9007199254740993}"#;
    let done = s
        .outcome(complete_step(&lease, 1, output), &key(1))
        .unwrap();
    assert_eq!(
        done,
        Outcome::StepCompleted {
            output: output.to_vec()
        }
    );
    let ended = s.run_cmd(complete(&lease, 1), &key(1)).unwrap();
    let (outcome, entries, events) = ended.into_parts();
    assert_eq!(
        outcome,
        Outcome::RunEnded(RunEnd {
            run: run.id,
            status: Status::Completed
        })
    );
    assert!(matches!(entries.as_slice(), [Entry::RunCompleted(c)] if c.run() == run.id));
    assert_eq!(
        events.len(),
        1,
        "the run's end comes with its event (RUN-2)"
    );
    let run = s.runs.values().next().unwrap();
    assert_eq!(run.status, Status::Completed);
    assert_eq!(run.output.as_deref(), Some(b"null".as_slice()));
}

/// RUN-2: an ended run answers `ended` to a begin and a completion, appending nothing; a step's
/// completion gets the step's outcome, or `Conflict`.
#[test]
fn an_ended_run_appends_nothing() {
    let (mut s, lease) = claimed();
    let lease = lease.to_string();
    s.outcome(begin(&lease, 1, "a"), &key(1)).unwrap();
    s.outcome(complete_step(&lease, 1, b"1"), &key(1)).unwrap();
    s.outcome(complete(&lease, 1), &key(1)).unwrap();
    let run = s.runs.values().next().unwrap().id;
    let end = RunEnd {
        run,
        status: Status::Completed,
    };
    for (cmd, answer) in [
        (complete(&lease, 1), Outcome::RunEnded(end.clone())),
        (complete(&lease, 5), Outcome::RunEnded(end.clone())),
        (
            begin(&lease, 2, "b"),
            Outcome::StepBegun(Begun::Ended(end.clone())),
        ),
    ] {
        let (outcome, entries, events) = s.run_cmd(cmd.clone(), &key(1)).unwrap().into_parts();
        assert!(entries.is_empty() && events.is_empty(), "{cmd:?}");
        assert_eq!(outcome, answer, "{cmd:?}");
    }
    let first = s.outcome(complete_step(&lease, 1, b"2"), &key(1)).unwrap();
    assert_eq!(
        first,
        Outcome::StepCompleted {
            output: b"1".to_vec()
        }
    );
    let over = s
        .outcome(complete_step(&lease, 2, b"2"), &key(1))
        .unwrap_err();
    assert_eq!(
        kind_and_hint(&over),
        (ErrorKind::Conflict, Some("this execution is over"))
    );
}

/// RUN-1: the fence's order: the lease and its run, then the execution, then the key. Each
/// refusal appends nothing.
#[test]
fn the_fence_answers_its_first_failure() {
    let (mut s, lease) = claimed();
    let unknown = Lease {
        run: "0190b5f0-0000-7000-8000-000000000999".parse().unwrap(),
        execution: 1,
    };
    let stale = Lease {
        execution: 2,
        ..lease
    };
    let superseded = (
        ErrorKind::Conflict,
        Some("this execution was superseded: stop working on this run"),
    );
    let foreign = (ErrorKind::Conflict, Some("another key holds this lease"));
    let cases = [
        (
            "not-a-lease".to_owned(),
            key(1),
            (ErrorKind::NotFound, None),
        ),
        (
            format!("{}.0", lease.run),
            key(1),
            (ErrorKind::NotFound, None),
        ),
        (unknown.to_string(), key(1), (ErrorKind::NotFound, None)),
        (stale.to_string(), key(1), superseded),
        (lease.to_string(), key(2), foreign),
        (stale.to_string(), key(2), superseded),
    ];
    for (lease, by, expected) in cases {
        for cmd in [
            begin(&lease, 1, "a"),
            complete_step(&lease, 1, b"1"),
            complete(&lease, 0),
        ] {
            let before = s.runs.clone();
            let refused = s.outcome(cmd.clone(), &by).unwrap_err();
            assert_eq!(kind_and_hint(&refused), expected, "{cmd:?} as {by}");
            assert_eq!(s.runs, before, "{cmd:?}");
            assert!(s.steps.is_empty());
        }
    }
    // A run never claimed is at execution 0, which no lease names.
    s.outcome(start(), &key(1)).unwrap();
    let queued = s.runs.values().find(|r| r.execution == 0).unwrap().id;
    let lease = Lease {
        run: queued,
        execution: 1,
    };
    let refused = s
        .outcome(complete(&lease.to_string(), 0), &key(1))
        .unwrap_err();
    assert_eq!(kind_and_hint(&refused), superseded);
}

/// What the request alone shows is answered before the fence.
#[test]
fn a_malformed_call_is_invalid_before_the_fence() {
    let (mut s, _) = claimed();
    for cmd in [
        begin("not-a-lease", 0, "a"),
        begin("not-a-lease", 1, ""),
        begin("not-a-lease", 1, "a\nb"),
        begin("not-a-lease", 1, &"x".repeat(257)),
        complete_step("not-a-lease", 0, b"1"),
        complete_step("not-a-lease", 1, b"{"),
        Command::Complete {
            lease: "not-a-lease".into(),
            steps: 0,
            output: Some(b"nope".to_vec()),
        },
    ] {
        let refused = s.outcome(cmd.clone(), &key(1)).unwrap_err();
        assert_eq!(refused.kind(), ErrorKind::Invalid, "{cmd:?}");
    }
    let name = "é".repeat(128);
    let at_limit = s
        .outcome(begin("not-a-lease", 1, &name), &key(1))
        .unwrap_err();
    assert_eq!(at_limit.kind(), ErrorKind::NotFound, "256 bytes is a name");
}

/// The Steps table's rows that need no second execution, and the first outcome standing.
#[test]
fn a_step_answers_by_its_position() {
    let (mut s, lease) = claimed();
    let lease = lease.to_string();
    let invalid = |s: &mut Store, cmd: Command| {
        let refused = s.outcome(cmd.clone(), &key(1)).unwrap_err();
        assert_eq!(refused.kind(), ErrorKind::Invalid, "{cmd:?}");
    };
    invalid(&mut s, begin(&lease, 2, "a"));
    invalid(&mut s, complete_step(&lease, 1, b"1"));
    s.outcome(begin(&lease, 1, "a"), &key(1)).unwrap();
    // Open, begun by this execution: run again, appending nothing.
    let again = s
        .run_cmd(begin(&lease, 1, "a"), &key(1))
        .unwrap()
        .into_parts();
    assert_eq!(again.0, Outcome::StepBegun(Begun::Run { attempt: 1 }));
    assert!(again.1.is_empty());
    invalid(&mut s, begin(&lease, 2, "b"));
    invalid(&mut s, complete(&lease, 1));
    invalid(&mut s, complete(&lease, 2));
    invalid(&mut s, complete_step(&lease, 2, b"1"));
    let output = br#"{"id":  9007199254740993}"#;
    s.outcome(complete_step(&lease, 1, output), &key(1))
        .unwrap();
    // Recorded: that output, byte for byte, appending nothing; the first outcome stands.
    let recorded = output.to_vec();
    for (cmd, answer) in [
        (
            begin(&lease, 1, "a"),
            Outcome::StepBegun(Begun::Completed {
                output: recorded.clone(),
            }),
        ),
        (
            complete_step(&lease, 1, b"2"),
            Outcome::StepCompleted {
                output: recorded.clone(),
            },
        ),
    ] {
        let (outcome, entries, _) = s.run_cmd(cmd.clone(), &key(1)).unwrap().into_parts();
        assert!(entries.is_empty(), "{cmd:?}");
        assert_eq!(outcome, answer, "{cmd:?}");
    }
    invalid(&mut s, begin(&lease, 3, "c"));
    // Rows that fail the run once #224 lands answer `Invalid` until then, appending nothing.
    invalid(&mut s, begin(&lease, 1, "renamed"));
    let next = s.outcome(begin(&lease, 2, "b"), &key(1)).unwrap();
    assert_eq!(next, Outcome::StepBegun(Begun::Run { attempt: 1 }));
}

/// LED-3 to LED-5, extended: outputs meet event data's rules, with messages of their own that
/// name the limit.
#[test]
fn an_output_follows_the_event_data_rules() {
    let (mut s, lease) = claimed();
    let lease = lease.to_string();
    s.outcome(begin(&lease, 1, "a"), &key(1)).unwrap();
    let largest = format!("\"{}\"", "x".repeat(MAX_DATA_BYTES - 2)).into_bytes();
    let over = format!("\"{}\"", "x".repeat(MAX_DATA_BYTES - 1)).into_bytes();
    for (output, refused) in [
        (over.as_slice(), "step output exceeds 524288 bytes"),
        (
            br#"{"a": 1, "a": 2}"#.as_slice(),
            "step output is invalid: ",
        ),
        (
            br#"{"$redacted": true}"#.as_slice(),
            "step output is invalid: ",
        ),
        (b"not json".as_slice(), "step output is invalid: "),
    ] {
        let r = s
            .outcome(complete_step(&lease, 1, output), &key(1))
            .unwrap_err();
        assert_eq!(r.kind(), ErrorKind::Invalid);
        assert!(r.message().starts_with(refused), "{}", r.message());
    }
    let done = s
        .outcome(complete_step(&lease, 1, &largest), &key(1))
        .unwrap();
    assert_eq!(
        done,
        Outcome::StepCompleted {
            output: largest.clone()
        }
    );
    let r = s
        .outcome(
            Command::Complete {
                lease: lease.clone(),
                steps: 1,
                output: Some(over),
            },
            &key(1),
        )
        .unwrap_err();
    assert_eq!(r.message(), "run output exceeds 524288 bytes");
    let ended = s.outcome(
        Command::Complete {
            lease,
            steps: 1,
            output: Some(largest.clone()),
        },
        &key(1),
    );
    let run = s.runs.values().next().unwrap().id;
    let end = RunEnd {
        run,
        status: Status::Completed,
    };
    assert_eq!(ended, Ok(Outcome::RunEnded(end)));
    assert_eq!(s.runs.values().next().unwrap().output, Some(largest));
}

/// RUN-7: a claim takes only a run of a pair it named, the earliest due first; a run keeps its
/// label when the function is put again.
#[test]
fn a_claim_takes_only_a_pair_it_named() {
    let mut s = Store {
        time: T,
        ..Store::default()
    };
    s.outcome(Command::PutFunction(definition()), &key(1))
        .unwrap();
    s.outcome(start(), &key(1)).unwrap();
    let v2 = Definition {
        code: "v2".into(),
        ..definition()
    };
    s.outcome(Command::PutFunction(v2), &key(1)).unwrap();
    s.outcome(start(), &key(1)).unwrap();
    let take = |s: &mut Store, pairs: &[&str]| match s.outcome(claim(pairs), &key(1)).unwrap() {
        Outcome::Claimed(c) => c.map(|c| (c.code, c.lease.execution)),
        other => panic!("{other:?}"),
    };
    assert_eq!(take(&mut s, &["refund@v1", "charge@v3"]), None);
    assert_eq!(take(&mut s, &["charge@v2"]), Some(("v2".into(), 1)));
    assert_eq!(take(&mut s, &["charge@v2"]), None);
    assert_eq!(
        take(&mut s, &["charge@v1", "charge@v2"]),
        Some(("v1".into(), 1))
    );
    let codes: Vec<_> = s.runs.values().map(|r| r.code.as_str()).collect();
    assert_eq!(
        codes,
        ["v1", "v2"].map(str::to_owned).iter().collect::<Vec<_>>()
    );
}

/// A claim's request outside the rules is `Invalid`, and draws nothing.
#[test]
fn a_claim_outside_the_rules_is_refused() {
    let (mut s, _) = claimed();
    let many: Vec<String> = (0..101).map(|i| format!("f{i}@v1")).collect();
    let many: Vec<&str> = many.iter().map(String::as_str).collect();
    for cmd in [
        claim(&[]),
        claim(&many),
        claim(&["charge"]),
        claim(&["charge@v1", "charge@v1"]),
        claim(&["a@b@c"]),
        Command::Claim {
            functions: vec!["charge@v1".into()],
            lease_seconds: Some(9),
            wait_seconds: None,
        },
        Command::Claim {
            functions: vec!["charge@v1".into()],
            lease_seconds: Some(901),
            wait_seconds: None,
        },
        Command::Claim {
            functions: vec!["charge@v1".into()],
            lease_seconds: None,
            wait_seconds: Some(21),
        },
    ] {
        let refused = s.outcome(cmd.clone(), &key(1)).unwrap_err();
        assert_eq!(refused.kind(), ErrorKind::Invalid, "{cmd:?}");
        // A waiting claim checks it the same way before it reads.
        let refused = claim_pairs(&cmd).unwrap_err();
        assert_eq!(refused.kind(), ErrorKind::Invalid, "{cmd:?}");
    }
    let hundred = &many[..100];
    assert_eq!(
        s.outcome(claim(hundred), &key(1)),
        Ok(Outcome::Claimed(None))
    );
    for (lease, wait) in [(10, 0), (900, 20)] {
        let cmd = Command::Claim {
            functions: vec!["x@v1".into()],
            lease_seconds: Some(lease),
            wait_seconds: Some(wait),
        };
        assert_eq!(claim_pairs(&cmd), Ok(vec![("x".into(), "v1".into())]));
        assert_eq!(s.outcome(cmd, &key(1)), Ok(Outcome::Claimed(None)));
    }
}

/// A waiting claim's pairs: in name order, as the claim's query takes them; a command that is no
/// claim is `Invalid`.
#[test]
fn a_waiting_claims_pairs_are_its_own() {
    let pairs = claim_pairs(&claim(&["refund@v2", "charge@v1"])).unwrap();
    let pair = |n: &str, c: &str| (n.to_owned(), c.to_owned());
    assert_eq!(pairs, [pair("charge", "v1"), pair("refund", "v2")]);
    assert_eq!(
        claim_pairs(&start()).unwrap_err().kind(),
        ErrorKind::Invalid
    );
}

/// A put follows the function rules this release checks, and an identical one appends nothing.
#[test]
fn a_put_checks_its_names_and_triggers() {
    let mut s = Store::default();
    let put = |s: &mut Store, d: Definition| s.run_cmd(Command::PutFunction(d), &key(1));
    for bad in [
        Definition {
            name: String::new(),
            ..definition()
        },
        Definition {
            name: "a@b".into(),
            ..definition()
        },
        Definition {
            name: "x".repeat(129),
            ..definition()
        },
        Definition {
            code: String::new(),
            ..definition()
        },
        Definition {
            triggers: (0..11).map(|i| format!("e.{i}")).collect(),
            ..definition()
        },
        Definition {
            triggers: vec!["a".into(), "a".into()],
            ..definition()
        },
        Definition {
            triggers: vec!["order..placed".into()],
            ..definition()
        },
        Definition {
            triggers: vec!["ironflowx.run.completed".into()],
            ..definition()
        },
    ] {
        let refused = put(&mut s, bad.clone()).unwrap_err();
        assert_eq!(refused.kind(), ErrorKind::Invalid, "{bad:?}");
    }
    let longest = Definition {
        name: "x".repeat(128),
        triggers: vec!["b".into(), "a".into()],
        ..definition()
    };
    let (outcome, entries, _) = put(&mut s, longest.clone()).unwrap().into_parts();
    let Outcome::FunctionPut { function, changed } = outcome else {
        panic!("a put");
    };
    assert!(changed);
    assert_eq!(function.definition.triggers, ["a", "b"], "in name order");
    assert_eq!(entries.len(), 1);
    let (outcome, entries, _) = put(&mut s, longest).unwrap().into_parts();
    assert!(matches!(
        outcome,
        Outcome::FunctionPut { changed: false, .. }
    ));
    assert!(entries.is_empty(), "an identical put appends nothing");
}

/// A start of an unknown function is `NotFound` and draws no run ID.
#[test]
fn an_unknown_function_cannot_be_started() {
    let mut s = Store::default();
    let refused = s.outcome(start(), &key(1)).unwrap_err();
    assert_eq!(
        (refused.kind(), refused.message()),
        (ErrorKind::NotFound, "function not found")
    );
    assert_eq!(s.drawn, 0);
}

/// Each entry reads back as it was written, and bytes cut short read as none.
#[test]
fn entries_round_trip() {
    let (mut s, lease) = claimed();
    let lease = lease.to_string();
    let mut entries = Vec::new();
    for cmd in [
        begin(&lease, 1, "réserve"),
        complete_step(&lease, 1, b"[1]"),
        complete(&lease, 1),
    ] {
        entries.extend(s.run_cmd(cmd, &key(1)).unwrap().into_parts().1);
    }
    let put = Entry::FunctionPut(Definition {
        triggers: vec!["a".into()],
        max_attempts: Some(3),
        ..definition()
    });
    entries.push(put);
    // A release, a heartbeat's renewal, and a lapsed lease's loss, and the tenth's failure.
    let (mut s, lease) = claimed();
    let lease = lease.to_string();
    for cmd in [heartbeat(&[&lease]), release(&lease)] {
        entries.extend(s.run_cmd(cmd, &key(1)).unwrap().into_parts().1);
    }
    let (mut s, _) = lost(9);
    s.time += 60_000;
    entries.extend(
        s.run_cmd(claim(&["charge@v1"]), &key(2))
            .unwrap()
            .into_parts()
            .1,
    );
    for entry in entries {
        let mut bytes = Vec::new();
        encode(&entry.clone().into(), &mut bytes);
        let mut rest = bytes.as_slice();
        assert_eq!(decode(&mut rest).expect("an entry"), entry);
        assert!(rest.is_empty());
        for cut in 0..bytes.len() {
            assert_eq!(decode(&mut &bytes[..cut]), None, "{entry:?} cut at {cut}");
        }
    }
    assert_eq!(decode(&mut [0u8].as_slice()), None, "kind 0 is an event's");
}

/// A claim as a log written before leases holds it (kind 3: the run, the execution, the deadline)
/// reads back, and its fold gives the run the lease length its deadline was decided with.
#[test]
fn a_claim_written_before_leases_reads_back_with_its_length() {
    let (s, _) = claimed();
    let run = the_run(&s);
    let mut bytes = vec![3];
    bytes.extend_from_slice(&36u32.to_le_bytes());
    bytes.extend_from_slice(run.id.to_string().as_bytes());
    bytes.extend_from_slice(&2u32.to_le_bytes());
    bytes.extend_from_slice(&(T + 15_000).to_le_bytes());
    let mut rest = bytes.as_slice();
    let entry = decode(&mut rest).expect("a claim");
    assert!(rest.is_empty());
    let mut s = s;
    fold(&mut s, &entry, Timestamp::from_millis(T), &key(2)).unwrap();
    let run = the_run(&s);
    assert_eq!((run.execution, run.lease_seconds), (2, 15));
}

/// The Steps rows that fail the run once #224 lands answer `Invalid` here, and append nothing:
/// `CompleteRun` with fewer steps than were begun, and a begin past 1,000 steps; and `CompleteRun`
/// with more steps than were begun, the last one recorded.
#[test]
fn rows_that_will_fail_the_run_are_invalid_until_then() {
    let (mut s, lease) = claimed();
    let lease = lease.to_string();
    for p in 1..=1_000 {
        s.outcome(begin(&lease, p, "s"), &key(1)).unwrap();
        s.outcome(complete_step(&lease, p, b"1"), &key(1)).unwrap();
    }
    let before = s.runs.clone();
    for cmd in [
        begin(&lease, 1_001, "one-more"),
        complete(&lease, 999),
        complete(&lease, 1_001),
    ] {
        let refused = s.outcome(cmd.clone(), &key(1)).unwrap_err();
        assert_eq!(refused.kind(), ErrorKind::Invalid, "{cmd:?}");
    }
    assert_eq!(s.runs, before);
    assert_eq!(s.steps.len(), 1_000);
    let end = s.outcome(complete(&lease, 1_000), &key(1));
    assert!(matches!(
        end,
        Ok(Outcome::RunEnded(RunEnd {
            status: Status::Completed,
            ..
        }))
    ));
}

/// The edges no other test reaches: 10 triggers are taken; a claim's lease runs the length it
/// asks for, renewed after a third of it; a pair's name and label each meet the name rule; a step
/// input of 65,536 bytes is taken and one byte more is refused before the fence.
#[test]
fn the_edges_of_a_put_a_claim_and_a_step_input() {
    use ironflowx_core::limits::MAX_STEP_INPUT_BYTES;
    let mut s = Store {
        time: T,
        ..Store::default()
    };
    let ten = Definition {
        triggers: (0..10).map(|i| format!("e.{i}")).collect(),
        ..definition()
    };
    let put = s.outcome(Command::PutFunction(ten), &key(1)).unwrap();
    assert!(matches!(put, Outcome::FunctionPut { changed: true, .. }));
    assert_eq!(s.functions["charge"].definition.triggers.len(), 10);
    for pair in ["char ge@v1", "charge@v 1", "charge@é", "@v1", "charge@"] {
        let refused = s.outcome(claim(&[pair]), &key(1)).unwrap_err();
        assert_eq!(refused.kind(), ErrorKind::Invalid, "{pair}");
    }
    s.outcome(start(), &key(1)).unwrap();
    let cmd = Command::Claim {
        functions: vec!["charge@v1".into()],
        lease_seconds: Some(30),
        wait_seconds: None,
    };
    let Outcome::Claimed(Some(claim)) = s.outcome(cmd, &key(1)).unwrap() else {
        panic!("a claim");
    };
    assert_eq!(claim.lease_until, Timestamp::from_millis(T + 30_000));
    assert_eq!(claim.renew_after_seconds, 10);
    assert_eq!(
        s.runs.values().next().unwrap().lease_until,
        Some(claim.lease_until)
    );
    let lease = claim.lease.to_string();
    let input = |len: usize| Command::BeginStep {
        lease: lease.clone(),
        position: 1,
        name: "a".into(),
        once: false,
        input: Some(format!("\"{}\"", "x".repeat(len - 2)).into_bytes()),
    };
    let refused = s
        .outcome(input(MAX_STEP_INPUT_BYTES + 1), &key(2))
        .unwrap_err();
    assert_eq!(
        (refused.kind(), refused.message()),
        (ErrorKind::Invalid, "step input exceeds 65536 bytes"),
        "before the fence, which would answer another key"
    );
    let begun = s.outcome(input(MAX_STEP_INPUT_BYTES), &key(1)).unwrap();
    assert_eq!(begun, Outcome::StepBegun(Begun::Run { attempt: 1 }));
    assert_eq!(
        s.steps
            .values()
            .next()
            .unwrap()
            .input
            .as_ref()
            .map(Vec::len),
        Some(MAX_STEP_INPUT_BYTES)
    );
}

/// Past the last run sequence, or the last execution number, a start and a claim are refused, and
/// nothing is drawn or appended.
#[test]
fn sequences_and_execution_numbers_run_out_refused() {
    let mut s = Store {
        time: T,
        ..Store::default()
    };
    s.outcome(Command::PutFunction(definition()), &key(1))
        .unwrap();
    s.outcome(start(), &key(1)).unwrap();
    let run = s.runs.values_mut().next().unwrap();
    run.sequence = u64::MAX;
    run.execution = u32::MAX;
    let drawn = s.drawn;
    let refused = s.outcome(start(), &key(1)).unwrap_err();
    assert_eq!(
        (refused.kind(), refused.message()),
        (ErrorKind::Invalid, "run sequences are used up")
    );
    assert_eq!(s.drawn, drawn);
    let refused = s.outcome(claim(&["charge@v1"]), &key(1)).unwrap_err();
    assert_eq!(
        (refused.kind(), refused.message()),
        (ErrorKind::Invalid, "execution numbers are used up")
    );
    assert_eq!(s.runs.values().next().unwrap().status, Status::Queued);
}

/// RUN-10 in `decide`: a claim asks the store at the event clock's `now`, so a run due later is
/// not taken a millisecond before its due time, and is at it.
#[test]
fn a_claim_takes_no_run_before_it_is_due() {
    let mut s = Store {
        time: T,
        ..Store::default()
    };
    s.outcome(Command::PutFunction(definition()), &key(1))
        .unwrap();
    s.outcome(start(), &key(1)).unwrap();
    // Due later than now; no command of this slice makes one, a retry will.
    s.runs.values_mut().next().unwrap().due_at = Some(Timestamp::from_millis(T + 1_000));
    s.time = T + 999;
    assert_eq!(
        s.outcome(claim(&["charge@v1"]), &key(1)),
        Ok(Outcome::Claimed(None))
    );
    s.time = T + 1_000;
    let taken = s.outcome(claim(&["charge@v1"]), &key(1)).unwrap();
    assert!(matches!(taken, Outcome::Claimed(Some(_))), "{taken:?}");
}

// ---- Definitions: every rule, replace, list ----

fn put_def(s: &mut Store, d: Definition) -> Result<Outcome, Refusal> {
    s.outcome(Command::PutFunction(d), &key(1))
}

fn named_fn(name: &str, triggers: &[&str]) -> Definition {
    Definition {
        name: name.into(),
        triggers: triggers.iter().map(|t| (*t).to_owned()).collect(),
        ..definition()
    }
}

/// Each limit of the Functions table at its edge, taken, and one past it, refused with a message
/// that names the rule.
#[test]
fn each_policy_field_and_label_is_taken_at_its_edge_and_refused_past_it() {
    let with = |f: fn(&mut Definition)| {
        let mut d = definition();
        f(&mut d);
        d
    };
    let taken = [
        with(|d| d.code = "x".repeat(128)),
        with(|d| d.max_attempts = Some(1)),
        with(|d| d.max_attempts = Some(100)),
        with(|d| d.first_delay_seconds = Some(1)),
        // Above the default longest delay, which is not checked: only two fields put are.
        with(|d| d.first_delay_seconds = Some(3_600)),
        with(|d| d.max_delay_seconds = Some(1)),
        with(|d| d.max_delay_seconds = Some(86_400)),
        with(|d| (d.first_delay_seconds, d.max_delay_seconds) = (Some(60), Some(60))),
    ];
    for d in taken {
        let mut s = Store::default();
        let put = put_def(&mut s, d.clone());
        assert!(
            matches!(put, Ok(Outcome::FunctionPut { changed: true, .. })),
            "{d:?}: {put:?}"
        );
        assert_eq!(s.functions["charge"].definition, d, "stored as put");
    }
    let labels = "is 1 to 128 bytes of letters, digits, '.', '_' and '-'";
    let refused = [
        (
            with(|d| d.code = "x".repeat(129)),
            format!("a code label {labels}"),
        ),
        (
            with(|d| d.max_attempts = Some(0)),
            "max_attempts is 1 to 100".into(),
        ),
        (
            with(|d| d.max_attempts = Some(101)),
            "max_attempts is 1 to 100".into(),
        ),
        (
            with(|d| d.first_delay_seconds = Some(0)),
            "first_delay_seconds is 1 to 3600".into(),
        ),
        (
            with(|d| d.first_delay_seconds = Some(3_601)),
            "first_delay_seconds is 1 to 3600".into(),
        ),
        (
            with(|d| d.max_delay_seconds = Some(0)),
            "max_delay_seconds is 1 to 86400".into(),
        ),
        (
            with(|d| d.max_delay_seconds = Some(86_401)),
            "max_delay_seconds is 1 to 86400".into(),
        ),
        (
            with(|d| (d.first_delay_seconds, d.max_delay_seconds) = (Some(60), Some(59))),
            "max_delay_seconds may not be below first_delay_seconds".into(),
        ),
    ];
    for (d, message) in refused {
        let mut s = Store::default();
        let r = put_def(&mut s, d.clone()).unwrap_err();
        assert_eq!(
            (r.kind(), r.message()),
            (ErrorKind::Invalid, message.as_str()),
            "{d:?}"
        );
    }
}

/// A stored definition holds only the policy fields that were put (D99), and a put replaces the
/// whole definition: a field left out of the second put is gone.
#[test]
fn a_put_stores_only_the_policy_it_was_given_and_replaces_the_rest() {
    let mut s = Store::default();
    let first = Definition {
        triggers: vec!["order.placed".into()],
        max_attempts: Some(5),
        first_delay_seconds: Some(2),
        max_delay_seconds: Some(30),
        ..definition()
    };
    put_def(&mut s, first).unwrap();
    let second = Definition {
        code: "v2".into(),
        first_delay_seconds: Some(7),
        ..definition()
    };
    let (_, entries, _) = s
        .run_cmd(Command::PutFunction(second.clone()), &key(2))
        .unwrap()
        .into_parts();
    assert_eq!(entries, [Entry::FunctionPut(second.clone())]);
    let stored = &s.functions["charge"];
    // The first put's max_attempts and max_delay_seconds are gone.
    assert_eq!(stored.definition, second);
    assert_eq!(stored.put_by, key(2));
}

/// At most 20 functions per event name: the 21st is refused with a hint that names the event; one
/// of the 20 put again with new code, or another function triggered by another name, is taken. A
/// function counts once however it is put.
#[test]
fn an_event_name_triggers_at_most_20_functions() {
    let mut s = Store::default();
    for i in 0..20 {
        put_def(&mut s, named_fn(&format!("f{i:02}"), &["order.placed"])).unwrap();
    }
    let refusals = [
        named_fn("late", &["order.placed"]),
        named_fn("late", &["order.paid", "order.placed"]),
    ];
    for d in refusals {
        let r = put_def(&mut s, d.clone()).unwrap_err();
        assert_eq!(
            (r.kind(), r.message(), r.hint()),
            (
                ErrorKind::ResourceExhausted,
                "an event name triggers at most 20 functions",
                Some(
                    "\"order.placed\" triggers 20 other functions already: put 1 of them without it first"
                )
            ),
            "{d:?}"
        );
    }
    // An existing function that is not among the 20 may not join them either.
    put_def(&mut s, named_fn("late", &["order.paid"])).unwrap();
    let r = put_def(&mut s, named_fn("late", &["order.placed"])).unwrap_err();
    assert_eq!(r.kind(), ErrorKind::ResourceExhausted);
    let again = Definition {
        code: "v2".into(),
        ..named_fn("f07", &["order.placed"])
    };
    let put = put_def(&mut s, again).unwrap();
    assert!(matches!(put, Outcome::FunctionPut { changed: true, .. }));
    // One of the 20 leaves, and another takes its place.
    put_def(&mut s, named_fn("f00", &[])).unwrap();
    put_def(&mut s, named_fn("late", &["order.placed"])).unwrap();
    assert_eq!(s.triggered_by("order.placed", "").unwrap(), 20);
}

/// At most 1,000 functions per environment: the 1,001st is refused; one of the 1,000 is still put
/// again, changed.
#[test]
fn an_environment_holds_at_most_1000_functions() {
    let mut s = Store::default();
    for i in 0..1_000 {
        put_def(&mut s, named_fn(&format!("f{i:04}"), &[])).unwrap();
    }
    let r = put_def(&mut s, named_fn("one-more", &[])).unwrap_err();
    assert_eq!(
        (r.kind(), r.message(), r.hint()),
        (
            ErrorKind::ResourceExhausted,
            "an environment holds at most 1000 functions",
            None
        )
    );
    let changed = Definition {
        code: "v2".into(),
        ..named_fn("f0999", &[])
    };
    let put = put_def(&mut s, changed).unwrap();
    assert!(matches!(put, Outcome::FunctionPut { changed: true, .. }));
    assert_eq!(s.functions["f0999"].definition.code, "v2");
}

fn token(n: u8) -> ironflowx_core::IncarnationToken {
    format!("{n:032x}").parse().unwrap()
}

fn names(page: &ironflowx_execution::FunctionPage) -> Vec<&str> {
    let names = page.functions.iter().map(|f| f.definition.name.as_str());
    names.collect()
}

fn with_functions(n: usize) -> Store {
    let mut s = Store::default();
    for i in 0..n {
        put_def(&mut s, named_fn(&format!("f.{i:03}"), &[])).unwrap();
    }
    s
}

fn page(s: &Store, limit: Option<u32>, cursor: Option<&str>) -> ironflowx_execution::FunctionPage {
    let limit = limit.map(|l| std::num::NonZeroU32::new(l).unwrap());
    ironflowx_execution::list_functions(s, token(1), limit, cursor).unwrap()
}

/// Functions list by name, page by page; the last page, full or not, has no cursor, and an empty
/// environment lists none.
#[test]
fn functions_list_by_name_page_by_page() {
    let mut s = Store::default();
    let empty = page(&s, None, None);
    assert_eq!((empty.functions.len(), empty.next), (0, None));
    for name in ["c", "a", "b"] {
        put_def(&mut s, named_fn(name, &[])).unwrap();
    }
    let first = page(&s, Some(2), None);
    assert_eq!(names(&first), ["a", "b"]);
    assert_eq!(first.next, Some(format!("{}.b", token(1))));
    let second = page(&s, Some(2), first.next.as_deref());
    assert_eq!((names(&second), second.next.clone()), (vec!["c"], None));
    let full = page(&s, Some(3), None);
    assert_eq!(
        (names(&full), full.next.clone()),
        (vec!["a", "b", "c"], None)
    );
    // A page holds each function as stored.
    assert_eq!(first.functions[0], s.functions["a"]);
}

#[test]
fn the_function_page_limit_defaults_to_20_and_clamps_at_200() {
    let s = with_functions(250);
    assert_eq!(page(&s, None, None).functions.len(), 20);
    assert_eq!(page(&s, Some(200), None).functions.len(), 200);
    assert_eq!(page(&s, Some(201), None).functions.len(), 200);
    assert_eq!(page(&s, Some(500), None).functions.len(), 200);
    assert_eq!(page(&s, Some(1), None).functions.len(), 1);
}

/// A cursor names the last function returned, so a page goes on after it whatever is put since:
/// a name before it is not listed, one after it is.
#[test]
fn a_function_cursor_is_stable_while_functions_are_put() {
    let mut s = with_functions(4);
    let first = page(&s, Some(2), None);
    assert_eq!(names(&first), ["f.000", "f.001"]);
    put_def(&mut s, named_fn("f.0005", &[])).unwrap();
    put_def(&mut s, named_fn("f.004", &[])).unwrap();
    let second = page(&s, Some(2), first.next.as_deref());
    assert_eq!(names(&second), ["f.002", "f.003"]);
    let third = page(&s, Some(2), second.next.as_deref());
    assert_eq!((names(&third), third.next.clone()), (vec!["f.004"], None));
    // A cursor past every name lists nothing.
    let past = format!("{}.zzz", token(1));
    assert_eq!(page(&s, None, Some(&past)).functions.len(), 0);
}

#[test]
fn a_function_cursor_that_does_not_decode_or_names_another_incarnation_is_invalid() {
    let s = with_functions(3);
    let next = page(&s, Some(1), None).next.expect("more pages");
    let other =
        ironflowx_execution::list_functions(&s, token(2), std::num::NonZeroU32::new(1), None)
            .unwrap()
            .next
            .expect("more pages");
    let bad = [
        String::new(),
        "not-a-token".into(),
        format!("{}", token(1)),
        format!("{}.", token(1)),
        format!("{}.a@b", token(1)),
        format!("{}.{}", token(1), "x".repeat(129)),
        format!("+{next}"),
        format!("0{next}"),
        format!("{:031x}.f.000", 1),
        other,
    ];
    for cursor in &bad {
        match ironflowx_execution::list_functions(&s, token(1), None, Some(cursor)) {
            Err(Error::Refused(r)) => assert_eq!(
                (r.kind(), r.message()),
                (ErrorKind::Invalid, "invalid cursor"),
                "{cursor:?}"
            ),
            other => panic!("expected invalid for {cursor:?}, got {other:?}"),
        }
    }
    // The token as it prints, in lower case, only.
    let lettered = token(0xab);
    let upper = format!("{}.f.000", lettered.to_string().to_uppercase());
    let listed = ironflowx_execution::list_functions(&s, lettered, None, Some(&upper));
    let Err(Error::Refused(r)) = listed else {
        panic!("an upper-case token was taken: {listed:?}");
    };
    assert_eq!(
        (r.kind(), r.message()),
        (ErrorKind::Invalid, "invalid cursor")
    );
    let lower = format!("{lettered}.f.000");
    assert!(ironflowx_execution::list_functions(&s, lettered, None, Some(&lower)).is_ok());
    // A name of 128 bytes, the longest, is a cursor.
    let longest = format!("{}.{}", token(1), "x".repeat(128));
    assert_eq!(page(&s, None, Some(&longest)).functions.len(), 0);
}

/// The store, with one of its reads failing.
struct Failing<'a> {
    store: &'a Store,
    read: &'static str,
}

impl Failing<'_> {
    fn fail<T>(&self, read: &str, ok: Result<T, String>) -> Result<T, String> {
        if read == self.read {
            return Err(format!("{read} failed"));
        }
        ok
    }
}

impl ExecutionView for Failing<'_> {
    type Error = String;

    fn function(&self, name: &str) -> Result<Option<Function>, String> {
        self.store.function(name)
    }

    fn triggered_by(&self, event: &str, other_than: &str) -> Result<u64, String> {
        let ok = self.store.triggered_by(event, other_than);
        self.fail("triggered_by", ok)
    }

    fn function_count(&self) -> Result<u64, String> {
        self.fail("function_count", self.store.function_count())
    }

    fn functions_after(&self, after: Option<&str>, limit: u32) -> Result<Vec<Function>, String> {
        let ok = self.store.functions_after(after, limit);
        self.fail("functions_after", ok)
    }

    fn run(&self, id: RunId) -> Result<Option<Run>, String> {
        self.store.run(id)
    }

    fn step(&self, run: RunId, position: u32) -> Result<Step, String> {
        self.store.step(run, position)
    }

    fn last_run_sequence(&self) -> Result<u64, String> {
        self.store.last_run_sequence()
    }

    fn claimable(
        &self,
        pairs: &[(String, String)],
        now: Timestamp,
        skip: u32,
    ) -> Result<Option<Run>, String> {
        self.store.claimable(pairs, now, skip)
    }
}

/// A read the caps or a page need that fails is the store's error, never a put that passes a cap
/// or an empty page.
#[test]
fn a_failed_read_of_the_caps_or_a_page_is_the_stores_error() {
    let s = with_functions(1);
    let new = Command::PutFunction(named_fn("new", &["order.placed"]));
    for read in ["function_count", "triggered_by"] {
        let view = Failing { store: &s, read };
        let draws = Draws {
            run_ids: &mut std::iter::empty(),
        };
        let decided = decide(&view, &new, &key(1), now(T), draws);
        assert_eq!(decided, Err(Error::Store(format!("{read} failed"))));
    }
    let view = Failing {
        store: &s,
        read: "functions_after",
    };
    let page = ironflowx_execution::list_functions(&view, token(1), None, None);
    assert_eq!(page, Err(Error::Store("functions_after failed".into())));
}

/// A store already over a cap, as a log written before the caps replays. Over a name's cap: a put
/// that leaves the name over it is refused, one of those already there included, while an
/// identical put, or one that lets the name go, is taken. Over the environment's cap: no new
/// function is taken, while one already there still changes, which leaves it over the cap.
#[test]
fn a_store_already_over_a_cap_takes_no_put_that_adds_to_it() {
    let mut s = Store::default();
    for i in 0..21 {
        let d = named_fn(&format!("f{i:02}"), &["order.placed"]);
        let f = Function {
            definition: d,
            put_at: Timestamp::from_millis(T),
            put_by: key(1),
        };
        s.functions.insert(f.definition.name.clone(), f);
    }
    let changed = Definition {
        code: "v2".into(),
        ..named_fn("f00", &["order.placed"])
    };
    // The hint counts the other functions the event triggers, and how many of them must go.
    let r = put_def(&mut s, changed).unwrap_err();
    let hint =
        "\"order.placed\" triggers 20 other functions already: put 1 of them without it first";
    assert_eq!(
        (r.kind(), r.hint()),
        (ErrorKind::ResourceExhausted, Some(hint))
    );
    let late = put_def(&mut s, named_fn("late", &["order.placed"])).unwrap_err();
    let hint =
        "\"order.placed\" triggers 21 other functions already: put 2 of them without it first";
    assert_eq!(
        (late.kind(), late.hint()),
        (ErrorKind::ResourceExhausted, Some(hint))
    );
    let same = put_def(&mut s, named_fn("f00", &["order.placed"])).unwrap();
    assert!(matches!(same, Outcome::FunctionPut { changed: false, .. }));
    let freed = put_def(&mut s, named_fn("f00", &[])).unwrap();
    assert!(matches!(freed, Outcome::FunctionPut { changed: true, .. }));
    assert_eq!(s.functions["f00"].definition.triggers, Vec::<String>::new());
    // Over the environment's cap too: no new function, while one already there still changes.
    for i in 0..980 {
        let d = named_fn(&format!("g{i:03}"), &[]);
        let f = Function {
            definition: d,
            put_at: Timestamp::from_millis(T),
            put_by: key(1),
        };
        s.functions.insert(f.definition.name.clone(), f);
    }
    assert_eq!(s.functions.len(), 1_001);
    let r = put_def(&mut s, named_fn("new", &[])).unwrap_err();
    assert_eq!(
        (r.kind(), r.message()),
        (
            ErrorKind::ResourceExhausted,
            "an environment holds at most 1000 functions"
        )
    );
    let changed = Definition {
        code: "v2".into(),
        ..named_fn("g000", &[])
    };
    let put = put_def(&mut s, changed).unwrap();
    assert!(matches!(put, Outcome::FunctionPut { changed: true, .. }));
    assert_eq!(s.functions["g000"].definition.code, "v2");
}

/// The hint names the event that is full, as sent: another name than the usual one, with a quote,
/// a backslash and a letter outside ASCII, quoted and escaped as Rust's `Debug` prints it.
#[test]
fn the_hint_names_the_full_event_whatever_it_is() {
    let event = "commande.\"passée\\";
    let mut s = Store::default();
    for i in 0..20 {
        put_def(&mut s, named_fn(&format!("f{i:02}"), &[event])).unwrap();
    }
    let r = put_def(&mut s, named_fn("late", &[event])).unwrap_err();
    let hint = r#""commande.\"passée\\" triggers 20 other functions already: put 1 of them without it first"#;
    assert_eq!(r.hint(), Some(hint));
}

/// Which cap answers when more than one would: the environment's first, with no hint; then the
/// first full trigger in name order.
#[test]
fn the_environments_cap_answers_first_then_the_first_full_name() {
    let mut s = Store::default();
    for (i, event) in ["b.full", "a.full"].iter().enumerate() {
        for j in 0..20 {
            put_def(&mut s, named_fn(&format!("f{i}{j:02}"), &[event])).unwrap();
        }
    }
    let both = named_fn("late", &["b.full", "a.full"]);
    let r = put_def(&mut s, both.clone()).unwrap_err();
    let hint = "\"a.full\" triggers 20 other functions already: put 1 of them without it first";
    assert_eq!(r.hint(), Some(hint));
    for i in 0..960 {
        put_def(&mut s, named_fn(&format!("g{i:03}"), &[])).unwrap();
    }
    assert_eq!(s.functions.len(), 1_000);
    let r = put_def(&mut s, both).unwrap_err();
    assert_eq!(
        (r.kind(), r.message(), r.hint()),
        (
            ErrorKind::ResourceExhausted,
            "an environment holds at most 1000 functions",
            None
        )
    );
}

fn heartbeat(leases: &[&str]) -> Command {
    Command::Heartbeat {
        leases: leases.iter().map(|l| (*l).to_owned()).collect(),
    }
}

fn release(lease: &str) -> Command {
    Command::Release {
        lease: lease.to_owned(),
    }
}

fn the_run(s: &Store) -> Run {
    s.runs.values().next().expect("a run").clone()
}

/// A claim of "charge" by `by`, 10 seconds long: its lease, if it took a run.
fn claim_10(s: &mut Store, by: &Emitter) -> Option<Lease> {
    let cmd = Command::Claim {
        functions: vec!["charge@v1".into()],
        lease_seconds: Some(10),
        wait_seconds: None,
    };
    match s.outcome(cmd, by).unwrap() {
        Outcome::Claimed(claim) => claim.map(|c| c.lease),
        other => panic!("a claim answered {other:?}"),
    }
}

/// A store whose one run has lost `n` leases in a row, each 10 seconds long, and is held by its
/// last claim, of key 2: that claim's lease. The clock is at that claim.
fn lost(n: u32) -> (Store, Lease) {
    let (mut s, mut lease) = claimed();
    for _ in 0..n {
        s.time += 60_000;
        lease = claim_10(&mut s, &key(2)).expect("a lapsed lease is taken");
    }
    (s, lease)
}

/// RUN-8, RUN-9: a run whose lease's deadline has passed is claimed again, by another key, as the
/// next execution, counting one lost lease; not a millisecond before. The superseded execution's
/// calls are refused, and the new one's accepted.
#[test]
fn a_lapsed_lease_is_claimed_again_counting_one_lost_lease() {
    let (mut s, first) = claimed();
    let first = first.to_string();
    s.time += 59_999;
    assert_eq!(claim_10(&mut s, &key(2)), None, "the lease holds");
    s.time += 1;
    let decision = s.run_cmd(claim(&["charge@v1"]), &key(2)).unwrap();
    let (outcome, entries, events) = decision.into_parts();
    let run = the_run(&s);
    let Outcome::Claimed(Some(claim)) = outcome else {
        panic!("a claim: {outcome:?}");
    };
    assert_eq!(
        claim.lease,
        Lease {
            run: run.id,
            execution: 2
        }
    );
    assert!(matches!(
        entries.as_slice(),
        [
            Entry::LeaseLost { lost_leases: 1, .. },
            Entry::Claimed { execution: 2, .. }
        ]
    ));
    assert!(events.is_empty());
    assert_eq!(
        (run.status, run.execution, run.lost_leases, run.claimed_by),
        (Status::Running, 2, 1, Some(key(2)))
    );
    let superseded = s.run_cmd(begin(&first, 1, "a"), &key(1)).unwrap_err();
    let stop = "this execution was superseded: stop working on this run";
    assert_eq!(
        kind_and_hint(&superseded),
        (ErrorKind::Conflict, Some(stop))
    );
    let second = claim.lease.to_string();
    let begun = s.outcome(begin(&second, 1, "a"), &key(2)).unwrap();
    assert_eq!(begun, Outcome::StepBegun(Begun::Run { attempt: 1 }));
}

/// RUN-9: the tenth lost lease in a row fails the run with `ironflowx.executor_lost`, with its
/// event, its count at 10 and its execution unchanged; the claim goes on to the next run in the
/// same record. A step's output sets the count back to 0.
#[test]
fn the_tenth_lost_lease_fails_the_run_and_the_claim_goes_on() {
    let (mut s, lease) = lost(9);
    let failing = the_run(&s);
    assert_eq!((failing.lost_leases, failing.execution), (9, 10));
    s.time += 10_000;
    let Outcome::RunStarted { run: next, .. } = s.outcome(start(), &key(1)).unwrap() else {
        panic!("a run");
    };
    let decision = s.run_cmd(claim(&["charge@v1"]), &key(3)).unwrap();
    let (outcome, entries, events) = decision.into_parts();
    let Outcome::Claimed(Some(claim)) = outcome else {
        panic!("the next run is claimed: {outcome:?}");
    };
    assert_eq!(claim.lease.run, next, "due at the same time, created later");
    let [
        Entry::LeaseLost {
            run: lost_run,
            lost_leases: 10,
        },
        Entry::RunFailed(failed),
        Entry::Claimed { run: claimed, .. },
    ] = entries.as_slice()
    else {
        panic!("a loss, a failure and a claim: {entries:?}");
    };
    assert_eq!(
        (*lost_run, failed.run(), *claimed),
        (failing.id, failing.id, next)
    );
    let error = RunError {
        code: "ironflowx.executor_lost".into(),
        message: "the run lost 10 leases in a row without progress".into(),
    };
    assert_eq!(failed.error(), &error);
    assert_eq!(events.len(), 1, "the failure comes with its event (RUN-2)");
    let run = s.runs[&failing.id.to_string()].clone();
    assert_eq!(
        (run.status, run.execution, run.lost_leases, run.error),
        (Status::Failed, 10, 10, Some(error))
    );
    assert_eq!(
        (run.ended_at, run.lease_until),
        (Some(Timestamp::from_millis(s.time)), None)
    );
    // Its last execution is told the run has ended.
    let ended = s
        .outcome(begin(&lease.to_string(), 1, "a"), &key(2))
        .unwrap();
    let end = RunEnd {
        run: failing.id,
        status: Status::Failed,
    };
    assert_eq!(ended, Outcome::StepBegun(Begun::Ended(end)));

    // A step's output sets the count back to 0.
    let (mut s, lease) = lost(3);
    let lease = lease.to_string();
    s.outcome(begin(&lease, 1, "a"), &key(2)).unwrap();
    assert_eq!(the_run(&s).lost_leases, 3, "a begin is no progress");
    s.outcome(complete_step(&lease, 1, b"1"), &key(2)).unwrap();
    assert_eq!(the_run(&s).lost_leases, 0);
}

/// A claim that meets more runs to fail than it may fails 100, appends their failures, and is
/// empty; the next claim fails the rest.
#[test]
fn a_claim_fails_at_most_100_runs() {
    let (mut s, _) = claimed();
    for _ in 0..100 {
        s.outcome(start(), &key(1)).unwrap();
        claim_10(&mut s, &key(1)).expect("a due run");
    }
    // Seeded below the rules: every run has lost 9 leases, and its tenth has lapsed.
    for run in s.runs.values_mut() {
        run.lost_leases = 9;
    }
    s.time += 60_000;
    let (outcome, entries, events) = s
        .run_cmd(claim(&["charge@v1"]), &key(2))
        .unwrap()
        .into_parts();
    assert_eq!(outcome, Outcome::Claimed(None));
    assert_eq!((entries.len(), events.len()), (200, 100));
    let failed = s
        .runs
        .values()
        .filter(|r| r.status == Status::Failed)
        .count();
    assert_eq!(failed, 100);
    let (outcome, entries, _) = s
        .run_cmd(claim(&["charge@v1"]), &key(2))
        .unwrap()
        .into_parts();
    assert_eq!((outcome, entries.len()), (Outcome::Claimed(None), 2));
    assert!(s.runs.values().all(|r| r.status == Status::Failed));
}

/// `Heartbeat`: one record renews every running lease named, to now plus its length; each other
/// lease is its own refusal, in the fence's order, and one named twice, none or 101 is `Invalid`.
#[test]
fn a_heartbeat_renews_each_running_lease_and_refuses_the_rest() {
    let (mut s, current) = claimed();
    let current = current.to_string();
    for _ in 0..3 {
        s.outcome(start(), &key(1)).unwrap();
    }
    let short = claim_10(&mut s, &key(1)).unwrap();
    let released = claim_10(&mut s, &key(1)).unwrap().to_string();
    let ended = claim_10(&mut s, &key(1)).unwrap().to_string();
    s.outcome(complete(&ended, 0), &key(1)).unwrap();
    s.outcome(release(&released), &key(1)).unwrap();
    s.time += 1_000;
    let unknown = "0190b5f0-0000-7000-8000-000000000999.1";
    let short_text = short.to_string();
    let leases = [
        current.as_str(),
        released.as_str(),
        unknown,
        "not-a-lease",
        short_text.as_str(),
        ended.as_str(),
    ];
    let (outcome, entries, _) = s.run_cmd(heartbeat(&leases), &key(1)).unwrap().into_parts();
    let Outcome::Renewed(results) = outcome else {
        panic!("renewals: {outcome:?}");
    };
    // Each to now plus its own length, in input order.
    let until = Timestamp::from_millis(s.time + 60_000);
    let short_until = Timestamp::from_millis(s.time + 10_000);
    assert_eq!((&results[0], &results[4]), (&Ok(until), &Ok(short_until)));
    let refused: Vec<_> = results[1..4]
        .iter()
        .map(|r| kind_and_hint(r.as_ref().unwrap_err()))
        .collect();
    assert_eq!(
        refused,
        [
            (ErrorKind::Conflict, Some("this execution is over")),
            (ErrorKind::NotFound, None),
            (ErrorKind::NotFound, None),
        ]
    );
    let over = (ErrorKind::Conflict, Some("this execution is over"));
    assert_eq!(
        kind_and_hint(results[5].as_ref().unwrap_err()),
        over,
        "an ended run"
    );
    let first = the_run(&s).id;
    assert_eq!(
        entries,
        [
            Entry::LeaseRenewed {
                run: first,
                lease_until: until
            },
            Entry::LeaseRenewed {
                run: short.run,
                lease_until: short_until
            },
        ],
        "both renewals, in one record"
    );
    assert_eq!(the_run(&s).lease_until, Some(until));
    assert_eq!(
        s.runs[&short.run.to_string()].lease_until,
        Some(short_until)
    );
    // Another key's heartbeat renews nothing, and appends nothing.
    let (outcome, entries, _) = s
        .run_cmd(heartbeat(&[&current]), &key(2))
        .unwrap()
        .into_parts();
    let held = (ErrorKind::Conflict, Some("another key holds this lease"));
    let Outcome::Renewed(results) = outcome else {
        panic!("renewals: {outcome:?}");
    };
    assert_eq!(kind_and_hint(results[0].as_ref().unwrap_err()), held);
    assert!(entries.is_empty());
    let leases: Vec<_> = (0..101).map(|i| format!("{i}")).collect();
    let leases: Vec<_> = leases.iter().map(String::as_str).collect();
    for wrong in [&[][..], &[current.as_str(), current.as_str()], &leases] {
        let r = s.run_cmd(heartbeat(wrong), &key(1)).unwrap_err();
        assert_eq!(r.kind(), ErrorKind::Invalid, "{} leases", wrong.len());
    }
    let r = s.run_cmd(heartbeat(&leases[..100]), &key(1)).unwrap();
    let Outcome::Renewed(results) = r.into_parts().0 else {
        panic!("renewals");
    };
    assert_eq!(results.len(), 100, "100 leases are taken");
}

/// `ReleaseRun`: the run is queued, due now, nothing counted; sent again it answers the same and
/// appends nothing. Until the next claim, the released execution's calls fare as "The fence"
/// says; after it, they are superseded. An ended run's release is `Conflict`.
#[test]
fn a_release_makes_the_run_due_now_and_counts_nothing() {
    let (mut s, lease) = claimed();
    let lease = lease.to_string();
    s.outcome(begin(&lease, 1, "a"), &key(1)).unwrap();
    s.outcome(complete_step(&lease, 1, b"7"), &key(1)).unwrap();
    s.outcome(begin(&lease, 2, "b"), &key(1)).unwrap();
    s.time += 5_000;
    let (outcome, entries, _) = s.run_cmd(release(&lease), &key(1)).unwrap().into_parts();
    let due_at = Timestamp::from_millis(s.time);
    assert_eq!(outcome, Outcome::Released { due_at });
    assert!(matches!(entries.as_slice(), [Entry::Released { .. }]));
    let run = the_run(&s);
    assert_eq!(
        (run.status, run.due_at, run.lease_until, run.lost_leases),
        (Status::Queued, Some(due_at), None, 0)
    );
    s.time += 1_000;
    let again = s.run_cmd(release(&lease), &key(1)).unwrap().into_parts();
    assert_eq!((again.0, again.1.len()), (Outcome::Released { due_at }, 0));
    let over = (ErrorKind::Conflict, Some("this execution is over"));
    for cmd in [
        begin(&lease, 2, "b"),
        complete_step(&lease, 2, b"1"),
        complete(&lease, 2),
    ] {
        let r = s.run_cmd(cmd.clone(), &key(1)).unwrap_err();
        assert_eq!(kind_and_hint(&r), over, "{cmd:?}");
    }
    let recorded = s.outcome(complete_step(&lease, 1, b"8"), &key(1)).unwrap();
    let output = b"7".to_vec();
    assert_eq!(recorded, Outcome::StepCompleted { output });
    // Claimed at once by another key, nothing counted.
    let decision = s.run_cmd(claim(&["charge@v1"]), &key(2)).unwrap();
    let entries = decision.into_parts().1;
    assert!(matches!(
        entries.as_slice(),
        [Entry::Claimed { execution: 2, .. }]
    ));
    assert_eq!(the_run(&s).lost_leases, 0);
    let stop = "this execution was superseded: stop working on this run";
    let r = s.run_cmd(release(&lease), &key(1)).unwrap_err();
    assert_eq!(kind_and_hint(&r), (ErrorKind::Conflict, Some(stop)));
    // An ended run.
    let (mut s, lease) = claimed();
    let lease = lease.to_string();
    s.outcome(complete(&lease, 0), &key(1)).unwrap();
    let r = s.run_cmd(release(&lease), &key(1)).unwrap_err();
    assert_eq!(kind_and_hint(&r), over);
}

/// An accepted call that appends renews its lease to now plus its length, even past its deadline
/// when no claim took the run; one that appends nothing does not.
#[test]
fn an_accepted_call_that_appends_renews_its_lease() {
    let (mut s, lease) = claimed();
    let lease = lease.to_string();
    let renewed = |entries: &[Entry]| match entries {
        [_, Entry::LeaseRenewed { lease_until, .. }] => Some(*lease_until),
        _ => None,
    };
    s.time += 5_000;
    let entries = s
        .run_cmd(begin(&lease, 1, "a"), &key(1))
        .unwrap()
        .into_parts()
        .1;
    let until = Timestamp::from_millis(s.time + 60_000);
    assert_eq!(renewed(&entries), Some(until));
    s.time += 1_000;
    let entries = s
        .run_cmd(begin(&lease, 1, "a"), &key(1))
        .unwrap()
        .into_parts()
        .1;
    assert!(entries.is_empty(), "a replayed begin appends nothing");
    assert_eq!(the_run(&s).lease_until, Some(until));
    // Late, and nobody took the run.
    s.time += 120_000;
    let entries = s
        .run_cmd(complete_step(&lease, 1, b"1"), &key(1))
        .unwrap()
        .into_parts()
        .1;
    let until = Timestamp::from_millis(s.time + 60_000);
    assert_eq!(renewed(&entries), Some(until));
    assert_eq!(the_run(&s).lease_until, Some(until));
}
