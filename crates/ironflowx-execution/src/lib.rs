//! Ironflow X execution rules: what a function may be, who may work on a run, and what a step call
//! answers.
//!
//! [`decide`] checks a [`Command`] against the current state and returns what to append: entries,
//! and the engine's own events, which the ledger sequences with event rules; [`fold`] applies an
//! appended entry to the read store; [`get_function`], [`list_functions`] and [`get_run`] read
//! back. Nothing here does I/O, reads a clock or draws randomness (LED-15): state comes through
//! [`ExecutionView`], goes out through [`ExecutionTables`], and run IDs come from the caller's
//! [`Draws`].

// Purity (LED-15): forbid, so an inner #[allow] cannot switch these off.
#![forbid(unsafe_code)]
#![forbid(clippy::disallowed_methods, clippy::disallowed_types)]
#![forbid(clippy::print_stdout, clippy::print_stderr, clippy::dbg_macro)]

mod codec;

use std::collections::BTreeSet;
use std::num::NonZeroU32;
use std::ops::RangeInclusive;

use ironflowx_core::limits::{
    MAX_DATA_BYTES, MAX_FUNCTION_NAME_BYTES, MAX_STEP_INPUT_BYTES, MAX_STEP_NAME_BYTES,
};
use ironflowx_core::{Emitter, IncarnationToken, Lease, RunId, SequencedTime, Timestamp};
/// The types this crate's API names from event rules, so a caller needs no `ironflowx-events`.
pub use ironflowx_events::{EngineEmit, Error, Refusal};
use ironflowx_events::{EngineEvent, check_name, check_value, engine_event};

pub use codec::{decode, encode};

/// The most triggers a function holds.
const MAX_TRIGGERS: usize = 10;
/// The most functions one event name triggers (D100).
const MAX_PER_EVENT: u64 = 20;
/// The most functions an environment holds.
const MAX_FUNCTIONS: u64 = 1_000;
const MAX_ATTEMPTS: RangeInclusive<u32> = 1..=100;
const FIRST_DELAY_SECONDS: RangeInclusive<u32> = 1..=3_600;
const MAX_DELAY_SECONDS: RangeInclusive<u32> = 1..=86_400;
/// A page of functions: 20 when no limit is asked, at most 200.
const DEFAULT_PAGE: u32 = 20;
const MAX_PAGE: u32 = 200;
/// The most `name@code` pairs a claim names.
const MAX_PAIRS: usize = 100;
/// The most steps a run begins.
const MAX_STEPS: u32 = 1_000;
const LEASE_SECONDS: RangeInclusive<u32> = 10..=900;
const DEFAULT_LEASE_SECONDS: u32 = 60;
const WAIT_SECONDS: RangeInclusive<u32> = 0..=20;
/// The most leases a heartbeat names.
const MAX_LEASES: usize = 100;
/// Lost leases in a row without progress that fail a run (D97).
const MAX_LOST_LEASES: u32 = 10;
/// The most runs one claim fails for their lost leases before it stops, empty.
const MAX_FAILED_BY_CLAIM: u32 = 100;
/// The engine's error code for a run that keeps losing its executor.
const EXECUTOR_LOST: &str = "ironflowx.executor_lost";
/// A run's input until `StartRun` takes one (#222), and an absent output: JSON `null`.
const NULL: &[u8] = b"null";

/// A function's definition, as put. The policy fields are `None` when not put: a default is
/// applied when the policy is used, never stored (D99).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Definition {
    pub name: String,
    pub code: String,
    /// Exact event names, kept and listed in name order.
    pub triggers: Vec<String>,
    pub max_attempts: Option<u32>,
    pub first_delay_seconds: Option<u32>,
    pub max_delay_seconds: Option<u32>,
}

/// A stored function: its definition, with the time and attribution of the record that put it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Function {
    pub definition: Definition,
    pub put_at: Timestamp,
    pub put_by: Emitter,
}

/// A run's status. It changes only when a record commits (D105).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Status {
    Queued,
    Running,
    Completed,
    Failed,
}

impl Status {
    /// Its name on the wire and in the read store.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }

    /// The status of a name [`Self::as_str`] gives.
    pub fn from_name(name: &str) -> Option<Self> {
        [Self::Queued, Self::Running, Self::Completed, Self::Failed]
            .into_iter()
            .find(|s| s.as_str() == name)
    }
}

/// A run, as the read store holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub id: RunId,
    /// Creation order: a number per environment from 1.
    pub sequence: u64,
    pub function: String,
    /// The function's code label when the run was created; it never changes (RUN-7).
    pub code: String,
    pub status: Status,
    /// JSON text.
    pub input: Vec<u8>,
    pub created_at: Timestamp,
    pub created_by: Emitter,
    /// While queued.
    pub due_at: Option<Timestamp>,
    /// The current execution's number; 0 before the first claim.
    pub execution: u32,
    /// While running.
    pub lease_until: Option<Timestamp>,
    /// The current execution's lease length, which a renewal adds to now; 0 before the first
    /// claim.
    pub lease_seconds: u32,
    /// Who the current execution's claim was attributed to: the fence compares it.
    pub claimed_by: Option<Emitter>,
    /// Leases lost in a row without progress: a claim of the run while it was running counts
    /// one, and a step's output sets it back to 0.
    pub lost_leases: u32,
    /// How many steps have been begun in the run's life.
    pub steps: u32,
    /// The open step's name, if one is open.
    pub step: Option<String>,
    /// JSON text, once completed.
    pub output: Option<Vec<u8>>,
    /// The last error, once one is recorded.
    pub error: Option<RunError>,
    pub ended_at: Option<Timestamp>,
}

/// A run's error. A code that begins with `ironflowx.` is the engine's own (D111).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunError {
    pub code: String,
    pub message: String,
}

/// A step of a run, as its first begin recorded it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    /// From 1; with the name, its identity.
    pub position: u32,
    pub name: String,
    pub once: bool,
    /// JSON text, if one was sent.
    pub input: Option<Vec<u8>>,
    /// The execution that began it.
    pub execution: u32,
    /// Its recorded outcome, JSON text; `None` while open.
    pub output: Option<Vec<u8>>,
}

/// What a client asks of execution rules. Text a rule must check (a lease, a `name@code` pair)
/// stays text here, so [`decide`] answers it in the fence's order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    PutFunction(Definition),
    /// The run's input is `null` until `StartRun` takes one (#222).
    StartRun {
        function: String,
    },
    /// Takes at most one due run of the pairs it names. It never waits (#226 adds the wait).
    Claim {
        functions: Vec<String>,
        lease_seconds: Option<u32>,
        wait_seconds: Option<u32>,
    },
    BeginStep {
        lease: String,
        position: u32,
        name: String,
        once: bool,
        input: Option<Vec<u8>>,
    },
    CompleteStep {
        lease: String,
        position: u32,
        output: Option<Vec<u8>>,
    },
    /// `CompleteRun`.
    Complete {
        lease: String,
        steps: u32,
        output: Option<Vec<u8>>,
    },
    /// Renews each lease it names that is accepted, in one record; each answers on its own.
    Heartbeat {
        leases: Vec<String>,
    },
    /// `ReleaseRun`: the run is due now, and nothing is counted.
    Release {
        lease: String,
    },
}

/// One entry of a record. It carries every value `decide` computed; [`fold`] reads no policy. Its
/// time and attribution are the record's. No entry about a run but `RunCreated` carries a code
/// label (RUN-7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    FunctionPut(Definition),
    /// Queued, due at once.
    RunCreated {
        run: RunId,
        sequence: u64,
        function: String,
        code: String,
        input: Vec<u8>,
    },
    /// The run is running under `execution`, held by the record's attribution, for a lease of
    /// `lease_seconds`.
    Claimed {
        run: RunId,
        execution: u32,
        lease_until: Timestamp,
        lease_seconds: u32,
    },
    /// The current execution's lease runs until `lease_until`.
    LeaseRenewed {
        run: RunId,
        lease_until: Timestamp,
    },
    /// A claim took the run while it was running: it has lost `lost_leases` in a row.
    LeaseLost {
        run: RunId,
        lost_leases: u32,
    },
    /// The run is queued, due at the record's time; nothing is counted.
    Released {
        run: RunId,
    },
    StepBegan {
        run: RunId,
        position: u32,
        name: String,
        once: bool,
        input: Option<Vec<u8>>,
        execution: u32,
    },
    StepCompleted {
        run: RunId,
        position: u32,
        output: Vec<u8>,
    },
    RunCompleted(RunCompleted),
    RunFailed(RunFailed),
}

/// A run's completion. Only [`decide`] makes one, in the [`Decision`] that holds its end event
/// (RUN-2): its fields are private, and what [`decode`] reads back is a [`Stored`], from which no
/// entry comes out.
///
/// ```compile_fail,E0451
/// let _ = ironflowx_execution::RunCompleted { run: todo!(), output: Vec::new() };
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunCompleted {
    run: RunId,
    output: Vec<u8>,
}

impl RunCompleted {
    pub const fn run(&self) -> RunId {
        self.run
    }

    /// JSON text.
    pub fn output(&self) -> &[u8] {
        &self.output
    }
}

/// A run's failure. Only [`decide`] makes one, in the [`Decision`] that holds its end event
/// (RUN-2), as [`RunCompleted`].
///
/// ```compile_fail,E0451
/// let _ = ironflowx_execution::RunFailed { run: todo!(), error: todo!() };
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunFailed {
    run: RunId,
    error: RunError,
}

impl RunFailed {
    pub const fn run(&self) -> RunId {
        self.run
    }

    pub const fn error(&self) -> &RunError {
        &self.error
    }
}

/// An entry as a record holds it: what [`encode`] writes, [`decode`] reads and [`fold`] applies.
/// An [`Entry`] becomes one; nothing turns one back into an entry, so a decoded `RunCompleted`
/// joins no decision (RUN-2). `encode` takes any `Stored`: only the ledger's writer appends what it
/// encodes, and only what `decide` decided.
///
/// ```compile_fail,E0616
/// fn out(s: ironflowx_execution::Stored) -> ironflowx_execution::Entry { s.0 }
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stored(Entry);

impl From<Entry> for Stored {
    fn from(entry: Entry) -> Self {
        Self(entry)
    }
}

/// So a reader of records can say which entry it holds.
impl PartialEq<Entry> for Stored {
    fn eq(&self, entry: &Entry) -> bool {
        self.0 == *entry
    }
}

/// What a claim answers when it took a run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claim {
    pub lease: Lease,
    pub function: String,
    pub code: String,
    /// JSON text.
    pub input: Vec<u8>,
    pub lease_until: Timestamp,
    /// A third of the lease's length.
    pub renew_after_seconds: u32,
}

/// A run that has ended, as an answer names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunEnd {
    pub run: RunId,
    pub status: Status,
}

/// What `BeginStep` answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Begun {
    /// Do the work.
    Run { attempt: u32 },
    /// The step's recorded output, byte for byte.
    Completed { output: Vec<u8> },
    /// The run has ended.
    Ended(RunEnd),
}

/// What a command answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    FunctionPut {
        function: Function,
        changed: bool,
    },
    RunStarted {
        run: RunId,
        duplicate: bool,
    },
    /// `None`: nothing was due.
    Claimed(Option<Claim>),
    StepBegun(Begun),
    /// The step's recorded output: this call's, or the first one's.
    StepCompleted {
        output: Vec<u8>,
    },
    RunEnded(RunEnd),
    /// `Heartbeat`: one per lease, in input order; the new deadline, or why it was refused.
    Renewed(Vec<Result<Timestamp, Refusal>>),
    /// `ReleaseRun`: when the run is due.
    Released {
        due_at: Timestamp,
    },
}

/// What [`decide`] returns. Its parts come out together, so the entries and the engine events that
/// belong with them reach the ledger as one (RUN-2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    outcome: Outcome,
    entries: Vec<Entry>,
    engine_events: Vec<EngineEmit>,
}

impl Decision {
    /// The answer, the entries to append and the engine's events to sequence into the same record.
    /// Nothing is to be appended when both lists are empty.
    pub fn into_parts(self) -> (Outcome, Vec<Entry>, Vec<EngineEmit>) {
        (self.outcome, self.entries, self.engine_events)
    }
}

/// What [`decide`] may draw: one run ID per run it creates.
pub struct Draws<'a> {
    pub run_ids: &'a mut dyn Iterator<Item = RunId>,
}

/// The current state, as `decide` and the queries read it.
pub trait ExecutionView {
    type Error;
    fn function(&self, name: &str) -> Result<Option<Function>, Self::Error>;
    /// How many functions but `other_than` have `event` among their triggers.
    fn triggered_by(&self, event: &str, other_than: &str) -> Result<u64, Self::Error>;
    /// How many functions there are.
    fn function_count(&self) -> Result<u64, Self::Error>;
    /// Up to `limit` functions, in name order, each named after `after`, or from the first when
    /// `None`.
    fn functions_after(
        &self,
        after: Option<&str>,
        limit: u32,
    ) -> Result<Vec<Function>, Self::Error>;
    fn run(&self, id: RunId) -> Result<Option<Run>, Self::Error>;
    /// A step the run counts among those begun: a store that lacks it has failed.
    fn step(&self, run: RunId, position: u32) -> Result<Step, Self::Error>;
    /// The highest run sequence; 0 when there is no run.
    fn last_run_sequence(&self) -> Result<u64, Self::Error>;
    /// Up to `limit` runs a claim of `pairs` may take at `now`, in the order it tries them: of one
    /// of those `(name, code)` pairs, queued and due by `now`, or running with a lease whose
    /// deadline is by `now`; the earliest due first, a lapsed lease due at its deadline, then the
    /// first created.
    fn claimable(
        &self,
        pairs: &[(String, String)],
        now: Timestamp,
        limit: u32,
    ) -> Result<Vec<Run>, Self::Error>;
}

/// The read store, as `fold` writes it.
pub trait ExecutionTables {
    type Error;
    /// Inserts the function, or replaces the one of its name.
    fn put_function(&mut self, function: &Function) -> Result<(), Self::Error>;
    fn create_run(&mut self, run: &Run) -> Result<(), Self::Error>;
    /// The run is running under `execution` until `lease_until`, a lease of `lease_seconds`, held
    /// by `by`.
    fn claim(
        &mut self,
        run: RunId,
        execution: u32,
        lease_until: Timestamp,
        lease_seconds: u32,
        by: &Emitter,
    ) -> Result<(), Self::Error>;
    /// The running run's lease runs until `lease_until`.
    fn renew(&mut self, run: RunId, lease_until: Timestamp) -> Result<(), Self::Error>;
    /// The run has lost `lost_leases` in a row.
    fn lose_lease(&mut self, run: RunId, lost_leases: u32) -> Result<(), Self::Error>;
    /// The running run is queued, due `at`, with no lease deadline.
    fn release(&mut self, run: RunId, at: Timestamp) -> Result<(), Self::Error>;
    /// Records the step, open, and counts it among the run's steps.
    fn begin_step(&mut self, run: RunId, step: &Step) -> Result<(), Self::Error>;
    /// Records the step's output; no step is open after it, and the run has lost no lease in a
    /// row.
    fn complete_step(
        &mut self,
        run: RunId,
        position: u32,
        output: &[u8],
    ) -> Result<(), Self::Error>;
    /// The run is completed with `output`, ended `at`.
    fn complete_run(&mut self, run: RunId, output: &[u8], at: Timestamp)
    -> Result<(), Self::Error>;
    /// The run is failed with `error`, ended `at`.
    fn fail_run(&mut self, run: RunId, error: &RunError, at: Timestamp) -> Result<(), Self::Error>;
}

fn refused<E>(r: Refusal) -> Error<E> {
    Error::Refused(r)
}

fn invalid<E>(message: impl Into<String>) -> Error<E> {
    refused(Refusal::invalid(message))
}

/// Decides a command against the current state. `by` is the attribution its record will carry,
/// which the fence compares; `now` is the event-clock reading that stamps it.
///
/// # Errors
///
/// A refusal, or the view's error.
///
/// # Panics
///
/// If `draws` runs out.
pub fn decide<V: ExecutionView>(
    view: &V,
    cmd: &Command,
    by: &Emitter,
    now: SequencedTime,
    draws: Draws<'_>,
) -> Result<Decision, Error<V::Error>> {
    let now = now.timestamp();
    match cmd {
        Command::PutFunction(definition) => put_function(view, definition, now, by),
        Command::StartRun { function } => {
            let function = view
                .function(function)
                .map_err(Error::Store)?
                .ok_or_else(|| refused(Refusal::not_found("function not found")))?;
            let sequence = view.last_run_sequence().map_err(Error::Store)?;
            let sequence = sequence
                .checked_add(1)
                .ok_or_else(|| invalid("run sequences are used up"))?;
            let run = draws.run_ids.next().expect("draws yield a run ID per run");
            let entry = Entry::RunCreated {
                run,
                sequence,
                function: function.definition.name,
                code: function.definition.code,
                input: NULL.to_vec(),
            };
            let outcome = Outcome::RunStarted {
                run,
                duplicate: false,
            };
            answer(outcome, vec![entry])
        }
        Command::Claim {
            functions,
            lease_seconds,
            wait_seconds,
        } => {
            let pairs = pairs(functions)?;
            let lease_seconds = lease_seconds.unwrap_or(DEFAULT_LEASE_SECONDS);
            if !LEASE_SECONDS.contains(&lease_seconds) {
                return Err(invalid("a lease is 10 to 900 seconds"));
            }
            // shortcut: accepted, and the claim answers at once; #226 makes it wait.
            if wait_seconds.is_some_and(|w| !WAIT_SECONDS.contains(&w)) {
                return Err(invalid("a wait is 0 to 20 seconds"));
            }
            claim(view, &pairs, lease_seconds, now)
        }
        Command::BeginStep {
            lease,
            position,
            name,
            once,
            input,
        } => {
            if *position == 0 {
                return Err(invalid("a step position is from 1"));
            }
            let named = (1..=MAX_STEP_NAME_BYTES).contains(&name.len())
                && !name.chars().any(char::is_control);
            if !named {
                return Err(invalid(format!(
                    "a step name is 1 to {MAX_STEP_NAME_BYTES} bytes with no control character"
                )));
            }
            if let Some(input) = input {
                check_value("step input", input, MAX_STEP_INPUT_BYTES).map_err(refused)?;
            }
            let run = fence(view, lease, by)?;
            begin_step(view, &run, *position, name, *once, input.as_deref(), now)
        }
        Command::CompleteStep {
            lease,
            position,
            output,
        } => {
            if *position == 0 {
                return Err(invalid("a step position is from 1"));
            }
            let output = output.as_deref().unwrap_or(NULL);
            check_value("step output", output, MAX_DATA_BYTES).map_err(refused)?;
            let run = fence(view, lease, by)?;
            complete_step(view, &run, *position, output, now)
        }
        Command::Complete {
            lease,
            steps,
            output,
        } => {
            let output = output.as_deref().unwrap_or(NULL);
            check_value("run output", output, MAX_DATA_BYTES).map_err(refused)?;
            let run = fence(view, lease, by)?;
            complete(view, &run, *steps, output)
        }
        Command::Heartbeat { leases } => heartbeat(view, leases, by, now),
        Command::Release { lease } => {
            let fenced = fence(view, lease, by)?;
            let run = fenced.run();
            match run.status {
                Status::Running => {
                    let entry = Entry::Released { run: run.id };
                    answer(Outcome::Released { due_at: now }, vec![entry])
                }
                // shortcut: a queued run past its first claim was released, the one way back to
                // queued until retries (#225), which keep how an execution ended.
                Status::Queued => {
                    let due_at = run.due_at.expect("a queued run is due");
                    answer(Outcome::Released { due_at }, Vec::new())
                }
                Status::Completed | Status::Failed => Err(over()),
            }
        }
    }
}

/// A claim after its request's checks: the first run it may take, in the view's order. Taking a
/// running run counts a lost lease; the tenth in a row fails the run instead, with its event, and
/// the claim goes on to the next, in the same record, until it has failed
/// [`MAX_FAILED_BY_CLAIM`].
fn claim<V: ExecutionView>(
    view: &V,
    pairs: &[(String, String)],
    lease_seconds: u32,
    now: Timestamp,
) -> Result<Decision, Error<V::Error>> {
    let runs = view
        .claimable(pairs, now, MAX_FAILED_BY_CLAIM + 1)
        .map_err(Error::Store)?;
    let mut entries = Vec::new();
    let mut engine_events = Vec::new();
    let mut failed = 0;
    for run in runs {
        if failed == MAX_FAILED_BY_CLAIM {
            break;
        }
        if run.status == Status::Running {
            let lost_leases = run.lost_leases.saturating_add(1);
            entries.push(Entry::LeaseLost {
                run: run.id,
                lost_leases,
            });
            if lost_leases >= MAX_LOST_LEASES {
                let error = RunError {
                    code: EXECUTOR_LOST.to_owned(),
                    message: format!("the run lost {lost_leases} leases in a row without progress"),
                };
                engine_events.push(failure_event(&run, &error));
                entries.push(Entry::RunFailed(RunFailed { run: run.id, error }));
                failed += 1;
                continue;
            }
        }
        let execution = run
            .execution
            .checked_add(1)
            .ok_or_else(|| invalid("execution numbers are used up"))?;
        let lease_until = after(now, lease_seconds);
        let claim = Claim {
            lease: Lease {
                run: run.id,
                execution,
            },
            function: run.function,
            code: run.code,
            input: run.input,
            lease_until,
            renew_after_seconds: lease_seconds / 3,
        };
        entries.push(Entry::Claimed {
            run: run.id,
            execution,
            lease_until,
            lease_seconds,
        });
        return Ok(Decision {
            outcome: Outcome::Claimed(Some(claim)),
            entries,
            engine_events,
        });
    }
    Ok(Decision {
        outcome: Outcome::Claimed(None),
        entries,
        engine_events,
    })
}

/// `ironflowx.run.failed` for `run`. Function names and code labels hold no NUL (`label`), and
/// neither does an engine's error, so event rules take this data.
fn failure_event(run: &Run, error: &RunError) -> EngineEmit {
    engine_event(EngineEvent::RunFailed {
        run_id: run.id,
        function: run.function.clone(),
        code: run.code.clone(),
        error_code: error.code.clone(),
        error_message: error.message.clone(),
    })
}

/// `Heartbeat`: 1 to [`MAX_LEASES`] different leases, each through the fence; one record renews
/// every running one, and a refused one is its own item's error.
fn heartbeat<V: ExecutionView>(
    view: &V,
    leases: &[String],
    by: &Emitter,
    now: Timestamp,
) -> Result<Decision, Error<V::Error>> {
    if !(1..=MAX_LEASES).contains(&leases.len()) {
        return Err(invalid(format!(
            "a heartbeat names 1 to {MAX_LEASES} leases"
        )));
    }
    let mut named = BTreeSet::new();
    if let Some(twice) = leases.iter().find(|l| !named.insert(l.as_str())) {
        return Err(invalid(format!("{twice:?} is named twice")));
    }
    let mut entries = Vec::new();
    let mut results = Vec::new();
    for lease in leases {
        let result = match fence(view, lease, by) {
            Ok(fenced) if fenced.run().status == Status::Running => {
                let run = fenced.run();
                let lease_until = after(now, run.lease_seconds);
                entries.push(Entry::LeaseRenewed {
                    run: run.id,
                    lease_until,
                });
                Ok(lease_until)
            }
            Ok(_) => Err(over_refusal()),
            Err(Error::Refused(r)) => Err(r),
            Err(e) => return Err(e),
        };
        results.push(result);
    }
    answer(Outcome::Renewed(results), entries)
}

/// The renewal of a running run's lease: now plus its length.
fn renewal(run: &Run, now: Timestamp) -> Entry {
    Entry::LeaseRenewed {
        run: run.id,
        lease_until: after(now, run.lease_seconds),
    }
}

fn put_function<V: ExecutionView>(
    view: &V,
    definition: &Definition,
    now: Timestamp,
    by: &Emitter,
) -> Result<Decision, Error<V::Error>> {
    label("a function name", &definition.name)?;
    label("a code label", &definition.code)?;
    if definition.triggers.len() > MAX_TRIGGERS {
        return Err(invalid(format!(
            "a function has at most {MAX_TRIGGERS} triggers"
        )));
    }
    let mut triggers = BTreeSet::new();
    for trigger in &definition.triggers {
        check_name(trigger).map_err(refused)?;
        if !triggers.insert(trigger.clone()) {
            return Err(invalid(format!("the trigger {trigger:?} is repeated")));
        }
    }
    policy(definition)?;
    let definition = Definition {
        triggers: triggers.into_iter().collect(),
        ..definition.clone()
    };
    let current = view.function(&definition.name).map_err(Error::Store)?;
    if let Some(function) = current.as_ref().filter(|f| f.definition == definition) {
        let outcome = Outcome::FunctionPut {
            function: function.clone(),
            changed: false,
        };
        return answer(outcome, Vec::new());
    }
    // The caps hold after every put, so no emit meets more triggers than they allow (RUN-12). The
    // writer decides one command at a time, so no put lands between these reads and the append.
    if current.is_none() && view.function_count().map_err(Error::Store)? >= MAX_FUNCTIONS {
        let r = Refusal::resource_exhausted(format!(
            "an environment holds at most {MAX_FUNCTIONS} functions"
        ));
        return Err(refused(r));
    }
    for trigger in &definition.triggers {
        let others = view
            .triggered_by(trigger, &definition.name)
            .map_err(Error::Store)?;
        if others >= MAX_PER_EVENT {
            // More than one to free only in a store already over the cap (a log from before it).
            let free = others - MAX_PER_EVENT + 1;
            let r = Refusal::resource_exhausted(format!(
                "an event name triggers at most {MAX_PER_EVENT} functions"
            ))
            .with_hint(format!(
                "{trigger:?} triggers {others} other functions already: put {free} of them without it first"
            ));
            return Err(refused(r));
        }
    }
    let function = Function {
        definition: definition.clone(),
        put_at: now,
        put_by: by.clone(),
    };
    let outcome = Outcome::FunctionPut {
        function,
        changed: true,
    };
    answer(outcome, vec![Entry::FunctionPut(definition)])
}

/// The retry policy's ranges, each field as put; a default is never checked, as it is never
/// stored (D99).
fn policy<E>(d: &Definition) -> Result<(), Error<E>> {
    let fields = [
        ("max_attempts", d.max_attempts, MAX_ATTEMPTS),
        (
            "first_delay_seconds",
            d.first_delay_seconds,
            FIRST_DELAY_SECONDS,
        ),
        ("max_delay_seconds", d.max_delay_seconds, MAX_DELAY_SECONDS),
    ];
    for (what, value, range) in fields {
        if value.is_some_and(|v| !range.contains(&v)) {
            return Err(invalid(format!(
                "{what} is {} to {}",
                range.start(),
                range.end()
            )));
        }
    }
    if let (Some(first), Some(max)) = (d.first_delay_seconds, d.max_delay_seconds)
        && max < first
    {
        return Err(invalid(
            "max_delay_seconds may not be below first_delay_seconds",
        ));
    }
    Ok(())
}

/// A function name or a code label: 1 to [`MAX_FUNCTION_NAME_BYTES`] bytes of `[A-Za-z0-9._-]`,
/// so `name@code` splits one way.
fn label<E>(what: &str, text: &str) -> Result<(), Error<E>> {
    if is_label(text) {
        return Ok(());
    }
    Err(invalid(format!(
        "{what} is 1 to {MAX_FUNCTION_NAME_BYTES} bytes of letters, digits, '.', '_' and '-'"
    )))
}

fn is_label(text: &str) -> bool {
    (1..=MAX_FUNCTION_NAME_BYTES).contains(&text.len())
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// A claim's `name@code` pairs: 1 to [`MAX_PAIRS`], none repeated.
fn pairs<E>(functions: &[String]) -> Result<Vec<(String, String)>, Error<E>> {
    if !(1..=MAX_PAIRS).contains(&functions.len()) {
        return Err(invalid(format!(
            "a claim names 1 to {MAX_PAIRS} functions as name@code"
        )));
    }
    let mut pairs = BTreeSet::new();
    for pair in functions {
        let (name, code) = pair
            .split_once('@')
            .ok_or_else(|| invalid(format!("{pair:?} is not name@code")))?;
        label("a function name", name)?;
        label("a code label", code)?;
        if !pairs.insert((name.to_owned(), code.to_owned())) {
            return Err(invalid(format!("{pair:?} is named twice")));
        }
    }
    Ok(pairs.into_iter().collect())
}

/// `seconds` after `at`.
fn after(at: Timestamp, seconds: u32) -> Timestamp {
    Timestamp::from_millis(at.as_millis().saturating_add(i64::from(seconds) * 1_000))
}

use fenced::{Fenced, fence};

/// The fence, apart from the rules: its type's field is private to this module, so only [`fence`]
/// makes a [`Fenced`], and no rule of an execution call reaches a run without the fence (RUN-1).
mod fenced {
    use ironflowx_core::{Emitter, Lease};

    use super::{Error, ExecutionView, Refusal, Run, refused};

    /// A run an execution call may act on.
    pub(super) struct Fenced {
        run: Run,
    }

    impl Fenced {
        pub(super) const fn run(&self) -> &Run {
            &self.run
        }
    }

    /// The fence, in the decision that writes, and in its order: the lease parses and its run
    /// exists, else `NotFound`; its execution is the run's current one, else `Conflict`; the
    /// call's attribution is the claim's, else `Conflict`.
    pub(super) fn fence<V: ExecutionView>(
        view: &V,
        lease: &str,
        by: &Emitter,
    ) -> Result<Fenced, Error<V::Error>> {
        let not_found = || refused(Refusal::not_found("run not found"));
        let lease: Lease = lease.parse().map_err(|_| not_found())?;
        let run = view
            .run(lease.run)
            .map_err(Error::Store)?
            .ok_or_else(not_found)?;
        // A run never claimed is at execution 0, which no lease names.
        if lease.execution != run.execution {
            let r = Refusal::conflict("the lease is not the run's current execution")
                .with_hint("this execution was superseded: stop working on this run");
            return Err(refused(r));
        }
        if run.claimed_by.as_ref() != Some(by) {
            let r = Refusal::conflict("the lease is held by another key")
                .with_hint("another key holds this lease");
            return Err(refused(r));
        }
        Ok(Fenced { run })
    }
}

fn ended(run: &Run) -> RunEnd {
    RunEnd {
        run: run.id,
        status: run.status,
    }
}

fn over_refusal() -> Refusal {
    Refusal::conflict("the execution is over").with_hint("this execution is over")
}

fn over<E>() -> Error<E> {
    refused(over_refusal())
}

/// A decision with no engine event: every one but a run's end.
fn answer<E>(outcome: Outcome, entries: Vec<Entry>) -> Result<Decision, Error<E>> {
    Ok(Decision {
        outcome,
        entries,
        engine_events: Vec::new(),
    })
}

/// The step at `position`, which the run counts: begun that many.
fn step_at<V: ExecutionView>(view: &V, run: &Run, position: u32) -> Result<Step, Error<V::Error>> {
    view.step(run.id, position).map_err(Error::Store)
}

/// `BeginStep` after the fence: the Steps table (architecture.md, "Steps").
fn begin_step<V: ExecutionView>(
    view: &V,
    fenced: &Fenced,
    position: u32,
    name: &str,
    once: bool,
    input: Option<&[u8]>,
    now: Timestamp,
) -> Result<Decision, Error<V::Error>> {
    let run = fenced.run();
    match run.status {
        Status::Completed | Status::Failed => {
            return answer(Outcome::StepBegun(Begun::Ended(ended(run))), Vec::new());
        }
        Status::Queued => return Err(over()),
        Status::Running => {}
    }
    let n = run.steps;
    let begun = |attempt| Outcome::StepBegun(Begun::Run { attempt });
    if position <= n {
        let step = step_at(view, run, position)?;
        if step.name != name {
            // shortcut: #224 fails the run with ironflowx.step_order instead.
            return Err(invalid(format!(
                "step {position} is {:?}, not {name:?}",
                step.name
            )));
        }
        if let Some(output) = step.output {
            return answer(Outcome::StepBegun(Begun::Completed { output }), Vec::new());
        }
        if step.execution == run.execution {
            return answer(begun(1), Vec::new());
        }
        // shortcut: #224 begins a step an earlier execution left open again, or fails a `once`
        // step with ironflowx.unknown_outcome.
        return Err(invalid(format!(
            "step {position} was begun by an earlier execution"
        )));
    }
    if position > n.saturating_add(1) {
        return Err(invalid(format!(
            "step {position} is past the next position, {}",
            n.saturating_add(1)
        )));
    }
    if n > 0 && step_at(view, run, n)?.output.is_none() {
        return Err(invalid(format!("one step at a time: step {n} is open")));
    }
    if position > MAX_STEPS {
        // shortcut: #224 fails the run with ironflowx.too_many_steps instead.
        return Err(invalid(format!("a run has at most {MAX_STEPS} steps")));
    }
    let entry = Entry::StepBegan {
        run: run.id,
        position,
        name: name.to_owned(),
        once,
        input: input.map(<[u8]>::to_vec),
        execution: run.execution,
    };
    answer(begun(1), vec![entry, renewal(run, now)])
}

/// `CompleteStep` after the fence. The first outcome of a step stands.
fn complete_step<V: ExecutionView>(
    view: &V,
    fenced: &Fenced,
    position: u32,
    output: &[u8],
    now: Timestamp,
) -> Result<Decision, Error<V::Error>> {
    let run = fenced.run();
    let step = if position <= run.steps {
        Some(step_at(view, run, position)?)
    } else {
        None
    };
    if let Some(Step {
        output: Some(recorded),
        ..
    }) = step
    {
        return answer(Outcome::StepCompleted { output: recorded }, Vec::new());
    }
    if run.status != Status::Running {
        return Err(over());
    }
    match step {
        Some(step) if step.execution == run.execution => {
            let entry = Entry::StepCompleted {
                run: run.id,
                position,
                output: output.to_vec(),
            };
            let outcome = Outcome::StepCompleted {
                output: output.to_vec(),
            };
            answer(outcome, vec![entry, renewal(run, now)])
        }
        _ => Err(invalid(format!(
            "step {position} is not open in this execution"
        ))),
    }
}

/// `CompleteRun` after the fence: its `steps` count first, then the open step.
fn complete<V: ExecutionView>(
    view: &V,
    fenced: &Fenced,
    steps: u32,
    output: &[u8],
) -> Result<Decision, Error<V::Error>> {
    let run = fenced.run();
    match run.status {
        Status::Completed | Status::Failed => {
            return answer(Outcome::RunEnded(ended(run)), Vec::new());
        }
        Status::Queued => return Err(over()),
        Status::Running => {}
    }
    let n = run.steps;
    if steps > n {
        return Err(invalid(format!(
            "the run has begun {n} steps, fewer than {steps}"
        )));
    }
    if steps < n {
        // shortcut: #224 fails the run with ironflowx.step_order instead.
        return Err(invalid(format!(
            "the run has begun {n} steps, more than {steps}"
        )));
    }
    if n > 0 && step_at(view, run, n)?.output.is_none() {
        return Err(invalid(format!("step {n} is open")));
    }
    let completed = RunCompleted {
        run: run.id,
        output: output.to_vec(),
    };
    // Function names and code labels hold no NUL (`label`), so event rules take this data.
    let event = engine_event(EngineEvent::RunCompleted {
        run_id: run.id,
        function: run.function.clone(),
        code: run.code.clone(),
    });
    let end = RunEnd {
        run: run.id,
        status: Status::Completed,
    };
    Ok(Decision {
        outcome: Outcome::RunEnded(end),
        entries: vec![Entry::RunCompleted(completed)],
        engine_events: vec![event],
    })
}

/// Applies one appended entry to the read store. `time` and `by` are its record's.
///
/// # Errors
///
/// The tables' error.
pub fn fold<T: ExecutionTables>(
    tables: &mut T,
    Stored(entry): &Stored,
    time: Timestamp,
    by: &Emitter,
) -> Result<(), T::Error> {
    match entry {
        Entry::FunctionPut(definition) => tables.put_function(&Function {
            definition: definition.clone(),
            put_at: time,
            put_by: by.clone(),
        }),
        Entry::RunCreated {
            run,
            sequence,
            function,
            code,
            input,
        } => tables.create_run(&Run {
            id: *run,
            sequence: *sequence,
            function: function.clone(),
            code: code.clone(),
            status: Status::Queued,
            input: input.clone(),
            created_at: time,
            created_by: by.clone(),
            due_at: Some(time),
            execution: 0,
            lease_until: None,
            lease_seconds: 0,
            claimed_by: None,
            lost_leases: 0,
            steps: 0,
            step: None,
            output: None,
            error: None,
            ended_at: None,
        }),
        Entry::Claimed {
            run,
            execution,
            lease_until,
            lease_seconds,
        } => tables.claim(*run, *execution, *lease_until, *lease_seconds, by),
        Entry::LeaseRenewed { run, lease_until } => tables.renew(*run, *lease_until),
        Entry::LeaseLost { run, lost_leases } => tables.lose_lease(*run, *lost_leases),
        Entry::Released { run } => tables.release(*run, time),
        Entry::StepBegan {
            run,
            position,
            name,
            once,
            input,
            execution,
        } => tables.begin_step(
            *run,
            &Step {
                position: *position,
                name: name.clone(),
                once: *once,
                input: input.clone(),
                execution: *execution,
                output: None,
            },
        ),
        Entry::StepCompleted {
            run,
            position,
            output,
        } => tables.complete_step(*run, *position, output),
        Entry::RunCompleted(completed) => {
            tables.complete_run(completed.run, &completed.output, time)
        }
        Entry::RunFailed(failed) => tables.fail_run(failed.run, &failed.error, time),
    }
}

/// Reads one function by name. An unknown name is `NotFound`.
///
/// # Errors
///
/// `NotFound`, or the view's error.
pub fn get_function<V: ExecutionView>(view: &V, name: &str) -> Result<Function, Error<V::Error>> {
    view.function(name)
        .map_err(Error::Store)?
        .ok_or_else(|| refused(Refusal::not_found("function not found")))
}

/// One page of functions, in name order. `next` continues after the last one; `None` on the last
/// page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionPage {
    pub functions: Vec<Function>,
    pub next: Option<String>,
}

/// Lists functions in name order. `limit` is 20 when `None` and at most 200. A cursor that does
/// not decode, or names an incarnation other than `token`, is `Invalid`, as an events cursor is.
///
/// # Errors
///
/// `Invalid` for a bad cursor, or the view's error.
pub fn list_functions<V: ExecutionView>(
    view: &V,
    token: IncarnationToken,
    limit: Option<NonZeroU32>,
    cursor: Option<&str>,
) -> Result<FunctionPage, Error<V::Error>> {
    let after = cursor
        .map(|cursor| decode_cursor(cursor, token).ok_or_else(|| invalid("invalid cursor")))
        .transpose()?;
    let limit = limit.map_or(DEFAULT_PAGE, |l| l.get().min(MAX_PAGE));
    // One more than the page holds tells whether a page follows, so a full last page carries no
    // cursor.
    let mut functions = view
        .functions_after(after, limit + 1)
        .map_err(Error::Store)?;
    let more = functions.len() > limit as usize;
    functions.truncate(limit as usize);
    let last = functions.last().filter(|_| more);
    let next = last.map(|f| format!("{token}.{}", f.definition.name));
    Ok(FunctionPage { functions, next })
}

/// A cursor is `<incarnation token>.<last function name returned>`: an encoding, not a
/// signature. The token holds no `.`; a name may.
fn decode_cursor(cursor: &str, token: IncarnationToken) -> Option<&str> {
    let (t, name) = cursor.split_once('.')?;
    // Only the form this module writes: this incarnation's token as it prints, and a name a put
    // takes.
    (is_label(name) && t == token.to_string()).then_some(name)
}

/// Reads one run by ID. An unknown ID, or one that does not parse, is `NotFound`.
///
/// # Errors
///
/// `NotFound`, or the view's error.
pub fn get_run<V: ExecutionView>(view: &V, id: &str) -> Result<Run, Error<V::Error>> {
    let not_found = || refused(Refusal::not_found("run not found"));
    let id = id.parse().map_err(|_| not_found())?;
    view.run(id).map_err(Error::Store)?.ok_or_else(not_found)
}
