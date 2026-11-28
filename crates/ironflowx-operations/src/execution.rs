//! What the engine's execution operations ask of the ledger and answer for it: requests as
//! commands, outcomes as answers. It holds no rule of its own; refusals map as events' do.

use ironflowx_cell::Outcome;
use ironflowx_execution::{self as execution, Begun, Command, Definition, Status};
use ironflowx_proto::{
    BeginStepRequest, BeginStepResponse, Claim, ClaimRunRequest, CompleteRunRequest,
    CompleteStepRequest, CompleteStepResponse, Function, PutFunctionRequest, PutFunctionResponse,
    Run, RunEnd, RunStatus, StartRunRequest, StartRunResponse,
};

/// What to do about a call whose outcome is unknown.
pub(crate) const PUT_IF_UNKNOWN: &str = "reopen the engine, then put again only if this definition should still replace the current \
     one: another put may have landed since";
pub(crate) const START_IF_UNKNOWN: &str =
    "reopen the engine, then check first what landed: a retry starts a second run";
pub(crate) const CLAIM_IF_UNKNOWN: &str = "reopen the engine, then claim again: until leases expire (a later release), the run of a \
     lost claim is not claimed again";
pub(crate) const LEASE_IF_UNKNOWN: &str =
    "reopen the engine, then send the same call again with the same lease: it repeats no work";

pub(crate) fn put(req: PutFunctionRequest) -> Command {
    Command::PutFunction(Definition {
        name: req.name,
        code: req.code,
        triggers: req.triggers,
        max_attempts: req.max_attempts,
        first_delay_seconds: req.first_delay_seconds,
        max_delay_seconds: req.max_delay_seconds,
    })
}

pub(crate) fn start(req: StartRunRequest) -> Command {
    Command::StartRun {
        function: req.function,
    }
}

pub(crate) fn claim(req: ClaimRunRequest) -> Command {
    Command::Claim {
        functions: req.functions,
        lease_seconds: req.lease_seconds,
        wait_seconds: req.wait_seconds,
    }
}

pub(crate) fn begin(req: BeginStepRequest) -> Command {
    Command::BeginStep {
        lease: req.lease,
        position: req.position,
        name: req.name,
        once: req.once,
        input: req.input,
    }
}

pub(crate) fn complete_step(req: CompleteStepRequest) -> Command {
    Command::CompleteStep {
        lease: req.lease,
        position: req.position,
        output: req.output,
    }
}

pub(crate) fn complete(req: CompleteRunRequest) -> Command {
    Command::Complete {
        lease: req.lease,
        steps: req.steps,
        output: req.output,
    }
}

/// An execution command's outcome: the ledger answers each command with its own kind.
pub(crate) fn outcome(o: Outcome) -> execution::Outcome {
    match o {
        Outcome::Execution(o) => o,
        Outcome::Events(_) => unreachable!("a command answers as execution rules do"),
    }
}

pub(crate) fn put_answer(o: execution::Outcome) -> PutFunctionResponse {
    let execution::Outcome::FunctionPut { function, changed } = o else {
        unreachable!("a put answers FunctionPut: {o:?}")
    };
    PutFunctionResponse {
        function: self::function(function),
        changed,
    }
}

pub(crate) fn start_answer(o: execution::Outcome) -> StartRunResponse {
    let execution::Outcome::RunStarted { run, duplicate } = o else {
        unreachable!("a start answers RunStarted: {o:?}")
    };
    StartRunResponse {
        run_id: run,
        duplicate,
    }
}

pub(crate) fn claim_answer(o: execution::Outcome) -> Option<Claim> {
    let execution::Outcome::Claimed(claim) = o else {
        unreachable!("a claim answers Claimed: {o:?}")
    };
    claim.map(|c| Claim {
        lease: c.lease,
        function: c.function,
        code: c.code,
        input: Some(c.input),
        lease_until: c.lease_until,
        renew_after_seconds: c.renew_after_seconds,
    })
}

pub(crate) fn begin_answer(o: execution::Outcome) -> BeginStepResponse {
    let execution::Outcome::StepBegun(begun) = o else {
        unreachable!("a begin answers StepBegun: {o:?}")
    };
    match begun {
        Begun::Run { attempt } => BeginStepResponse::Run { attempt },
        Begun::Completed { output } => BeginStepResponse::Completed { output },
        Begun::Ended(end) => BeginStepResponse::Ended(self::end(end)),
    }
}

pub(crate) fn complete_step_answer(o: execution::Outcome) -> CompleteStepResponse {
    let execution::Outcome::StepCompleted { output } = o else {
        unreachable!("a step's completion answers StepCompleted: {o:?}")
    };
    CompleteStepResponse::Completed { output }
}

pub(crate) fn complete_answer(o: execution::Outcome) -> RunEnd {
    let execution::Outcome::RunEnded(end) = o else {
        unreachable!("a run's completion answers RunEnded: {o:?}")
    };
    self::end(end)
}

fn end(end: execution::RunEnd) -> RunEnd {
    RunEnd {
        run_id: end.run,
        status: status(end.status),
    }
}

const fn status(status: Status) -> RunStatus {
    match status {
        Status::Queued => RunStatus::Queued,
        Status::Running => RunStatus::Running,
        Status::Completed => RunStatus::Completed,
        Status::Failed => RunStatus::Failed,
    }
}

pub(crate) fn function(f: execution::Function) -> Function {
    let d = f.definition;
    Function {
        name: d.name,
        code: d.code,
        triggers: d.triggers,
        max_attempts: d.max_attempts,
        first_delay_seconds: d.first_delay_seconds,
        max_delay_seconds: d.max_delay_seconds,
        put_at: f.put_at,
        put_by: f.put_by,
    }
}

pub(crate) fn run(r: execution::Run) -> Run {
    Run {
        run_id: r.id,
        function: r.function,
        code: r.code,
        status: status(r.status),
        input: Some(r.input),
        created_at: r.created_at,
        created_by: r.created_by,
        due_at: r.due_at,
        execution: r.execution,
        lease_until: r.lease_until,
        steps: r.steps,
        step: r.step,
        output: r.output,
        ended_at: r.ended_at,
    }
}
