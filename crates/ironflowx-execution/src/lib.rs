//! Ironflow X execution rules: what a function may be, who may work on a run, and what a step call
//! answers.
//!
//! [`decide`] checks a [`Command`] against the current state and returns what to append: entries,
//! and the engine's own events, which the ledger sequences with event rules; [`fold`] applies an
//! appended entry to the read store; [`get_function`] and [`get_run`] read back. Nothing here does
//! I/O, reads a clock or draws randomness (LED-15): state comes through [`ExecutionView`], goes out
//! through [`ExecutionTables`], and run IDs come from the caller's [`Draws`].

// Purity (LED-15): forbid, so an inner #[allow] cannot switch these off.
#![forbid(unsafe_code)]
#![forbid(clippy::disallowed_methods, clippy::disallowed_types)]
#![forbid(clippy::print_stdout, clippy::print_stderr, clippy::dbg_macro)]

mod codec;

use std::collections::BTreeSet;

use ironflowx_core::limits::{
    MAX_DATA_BYTES, MAX_FUNCTION_NAME_BYTES, MAX_STEP_INPUT_BYTES, MAX_STEP_NAME_BYTES,
};
use ironflowx_core::{Emitter, Lease, RunId, SequencedTime, Timestamp};
/// The types this crate's API names from event rules, so a caller needs no `ironflowx-events`.
pub use ironflowx_events::{EngineEmit, Error, Refusal};
use ironflowx_events::{EngineEvent, check_name, check_value, engine_event};

pub use codec::{decode, encode};

/// The most triggers a function holds.
const MAX_TRIGGERS: usize = 10;
/// The most `name@code` pairs a claim names.
const MAX_PAIRS: usize = 100;
/// The most steps a run begins.
const MAX_STEPS: u32 = 1_000;
const LEASE_SECONDS: std::ops::RangeInclusive<u32> = 10..=900;
const DEFAULT_LEASE_SECONDS: u32 = 60;
const WAIT_SECONDS: std::ops::RangeInclusive<u32> = 0..=20;
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

    const fn ended(self) -> bool {
        matches!(self, Self::Completed | Self::Failed)
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
    /// Who the current execution's claim was attributed to: the fence compares it.
    pub claimed_by: Option<Emitter>,
    /// How many steps have been begun in the run's life.
    pub steps: u32,
    /// The open step's name, if one is open.
    pub step: Option<String>,
    /// JSON text, once completed.
    pub output: Option<Vec<u8>>,
    pub ended_at: Option<Timestamp>,
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
    /// The run is running under `execution`, held by the record's attribution.
    Claimed {
        run: RunId,
        execution: u32,
        lease_until: Timestamp,
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
    fn run(&self, id: RunId) -> Result<Option<Run>, Self::Error>;
    /// A step the run counts among those begun: a store that lacks it has failed.
    fn step(&self, run: RunId, position: u32) -> Result<Step, Self::Error>;
    /// The highest run sequence; 0 when there is no run.
    fn last_run_sequence(&self) -> Result<u64, Self::Error>;
    /// The run a claim of `pairs` takes at `now`: one of those `(name, code)` pairs, queued and due
    /// by `now`; the earliest due first, then the first created.
    fn claimable(
        &self,
        pairs: &[(String, String)],
        now: Timestamp,
    ) -> Result<Option<Run>, Self::Error>;
}

/// The read store, as `fold` writes it.
pub trait ExecutionTables {
    type Error;
    /// Inserts the function, or replaces the one of its name.
    fn put_function(&mut self, function: &Function) -> Result<(), Self::Error>;
    fn create_run(&mut self, run: &Run) -> Result<(), Self::Error>;
    /// The run is running under `execution` until `lease_until`, held by `by`.
    fn claim(
        &mut self,
        run: RunId,
        execution: u32,
        lease_until: Timestamp,
        by: &Emitter,
    ) -> Result<(), Self::Error>;
    /// Records the step, open, and counts it among the run's steps.
    fn begin_step(&mut self, run: RunId, step: &Step) -> Result<(), Self::Error>;
    /// Records the step's output; no step is open after it.
    fn complete_step(
        &mut self,
        run: RunId,
        position: u32,
        output: &[u8],
    ) -> Result<(), Self::Error>;
    /// The run is completed with `output`, ended `at`.
    fn complete_run(&mut self, run: RunId, output: &[u8], at: Timestamp)
    -> Result<(), Self::Error>;
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
            let Some(run) = view.claimable(&pairs, now).map_err(Error::Store)? else {
                return answer(Outcome::Claimed(None), Vec::new());
            };
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
            let entry = Entry::Claimed {
                run: run.id,
                execution,
                lease_until,
            };
            answer(Outcome::Claimed(Some(claim)), vec![entry])
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
            begin_step(view, &run, *position, name, *once, input.as_deref())
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
            complete_step(view, &run, *position, output)
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
    // shortcut: the policy ranges, and the caps per event name and per environment, are #220's.
    let definition = Definition {
        triggers: triggers.into_iter().collect(),
        ..definition.clone()
    };
    let current = view.function(&definition.name).map_err(Error::Store)?;
    if let Some(function) = current.filter(|f| f.definition == definition) {
        let outcome = Outcome::FunctionPut {
            function,
            changed: false,
        };
        return answer(outcome, Vec::new());
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

/// A function name or a code label: 1 to [`MAX_FUNCTION_NAME_BYTES`] bytes of `[A-Za-z0-9._-]`,
/// so `name@code` splits one way.
fn label<E>(what: &str, text: &str) -> Result<(), Error<E>> {
    let ok = (1..=MAX_FUNCTION_NAME_BYTES).contains(&text.len())
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    if ok {
        return Ok(());
    }
    Err(invalid(format!(
        "{what} is 1 to {MAX_FUNCTION_NAME_BYTES} bytes of letters, digits, '.', '_' and '-'"
    )))
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

fn over<E>() -> Error<E> {
    refused(Refusal::conflict("the execution is over").with_hint("this execution is over"))
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
) -> Result<Decision, Error<V::Error>> {
    let run = fenced.run();
    if run.status.ended() {
        return answer(Outcome::StepBegun(Begun::Ended(ended(run))), Vec::new());
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
        // shortcut: no step is open from an earlier execution until #223 lets a run be claimed
        // again; #224 begins it again, or fails a `once` step with ironflowx.unknown_outcome.
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
    answer(begun(1), vec![entry])
}

/// `CompleteStep` after the fence. The first outcome of a step stands.
fn complete_step<V: ExecutionView>(
    view: &V,
    fenced: &Fenced,
    position: u32,
    output: &[u8],
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
    if run.status.ended() {
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
            answer(outcome, vec![entry])
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
    if run.status.ended() {
        return answer(Outcome::RunEnded(ended(run)), Vec::new());
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
            claimed_by: None,
            steps: 0,
            step: None,
            output: None,
            ended_at: None,
        }),
        Entry::Claimed {
            run,
            execution,
            lease_until,
        } => tables.claim(*run, *execution, *lease_until, by),
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
