//! Execution rules against an in-memory read store: `decide`, then `fold` of what it decided.

use std::collections::BTreeMap;

use ironflowx_core::limits::MAX_DATA_BYTES;
use ironflowx_core::{Emitter, ErrorKind, Lease, RunId, SequencedTime, Timestamp};
use ironflowx_execution::{
    Begun, Command, Decision, Definition, Draws, Entry, Error, ExecutionTables, ExecutionView,
    Function, Outcome, Refusal, Run, RunEnd, Status, Step, decide, decode, encode, fold,
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

    fn claimable(&self, pairs: &[(String, String)], now: Timestamp) -> Result<Option<Run>, String> {
        let mut due: Vec<_> = self
            .runs
            .values()
            .filter(|r| pairs.contains(&(r.function.clone(), r.code.clone())))
            .filter(|r| r.status == Status::Queued && r.due_at.is_some_and(|d| d <= now))
            .collect();
        due.sort_by_key(|r| (r.due_at, r.sequence));
        Ok(due.first().map(|r| (*r).clone()))
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
        by: &Emitter,
    ) -> Result<(), String> {
        let r = self.runs.get_mut(&run.to_string()).unwrap();
        r.status = Status::Running;
        r.execution = execution;
        r.lease_until = Some(lease_until);
        r.claimed_by = Some(by.clone());
        r.due_at = None;
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
        self.runs.get_mut(&run.to_string()).unwrap().step = None;
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
        assert_eq!(s.outcome(cmd, &key(1)), Ok(Outcome::Claimed(None)));
    }
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
