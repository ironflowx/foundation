//! An entry's encoding in a record: its kind byte, then its fields, binary and length-prefixed, as
//! the ledger encodes an event. Kind 0 is the ledger's own, an event (D112).

use ironflowx_core::{RunId, Timestamp};

use crate::{Definition, Entry, RunCompleted, RunError, RunFailed, Stored};

const FUNCTION_PUT: u8 = 1;
const RUN_CREATED: u8 = 2;
const CLAIMED: u8 = 3;
const STEP_BEGAN: u8 = 4;
const STEP_COMPLETED: u8 = 5;
const RUN_COMPLETED: u8 = 6;
const LEASE_RENEWED: u8 = 7;
const LEASE_LOST: u8 = 8;
const RELEASED: u8 = 9;
const RUN_FAILED: u8 = 10;

/// Appends `entry` to `out`, led by its kind byte.
pub fn encode(Stored(entry): &Stored, out: &mut Vec<u8>) {
    let mut e = Enc(out);
    match entry {
        Entry::FunctionPut(d) => {
            e.byte(FUNCTION_PUT);
            e.bytes(d.name.as_bytes());
            e.bytes(d.code.as_bytes());
            e.u32(len(d.triggers.len()));
            for trigger in &d.triggers {
                e.bytes(trigger.as_bytes());
            }
            for field in [d.max_attempts, d.first_delay_seconds, d.max_delay_seconds] {
                e.opt_u32(field);
            }
        }
        Entry::RunCreated {
            run,
            sequence,
            function,
            code,
            input,
        } => {
            e.byte(RUN_CREATED);
            e.run(*run);
            e.0.extend_from_slice(&sequence.to_le_bytes());
            e.bytes(function.as_bytes());
            e.bytes(code.as_bytes());
            e.bytes(input);
        }
        Entry::Claimed {
            run,
            execution,
            lease_until,
            lease_seconds,
        } => {
            e.byte(CLAIMED);
            e.run(*run);
            e.u32(*execution);
            e.time(*lease_until);
            e.u32(*lease_seconds);
        }
        Entry::LeaseRenewed { run, lease_until } => {
            e.byte(LEASE_RENEWED);
            e.run(*run);
            e.time(*lease_until);
        }
        Entry::LeaseLost { run, lost_leases } => {
            e.byte(LEASE_LOST);
            e.run(*run);
            e.u32(*lost_leases);
        }
        Entry::Released { run } => {
            e.byte(RELEASED);
            e.run(*run);
        }
        Entry::StepBegan {
            run,
            position,
            name,
            once,
            input,
            execution,
        } => {
            e.byte(STEP_BEGAN);
            e.run(*run);
            e.u32(*position);
            e.bytes(name.as_bytes());
            e.byte(u8::from(*once));
            match input {
                None => e.byte(0),
                Some(input) => {
                    e.byte(1);
                    e.bytes(input);
                }
            }
            e.u32(*execution);
        }
        Entry::StepCompleted {
            run,
            position,
            output,
        } => {
            e.byte(STEP_COMPLETED);
            e.run(*run);
            e.u32(*position);
            e.bytes(output);
        }
        Entry::RunCompleted(RunCompleted { run, output }) => {
            e.byte(RUN_COMPLETED);
            e.run(*run);
            e.bytes(output);
        }
        Entry::RunFailed(RunFailed { run, error }) => {
            e.byte(RUN_FAILED);
            e.run(*run);
            e.bytes(error.code.as_bytes());
            e.bytes(error.message.as_bytes());
        }
    }
}

/// Execution rules bound every field they store, and the ledger's append cap bounds a record, so
/// every length fits.
fn len(n: usize) -> u32 {
    u32::try_from(n).expect("execution rules bound an entry's lengths")
}

struct Enc<'a>(&'a mut Vec<u8>);

impl Enc<'_> {
    fn byte(&mut self, b: u8) {
        self.0.push(b);
    }

    fn u32(&mut self, n: u32) {
        self.0.extend_from_slice(&n.to_le_bytes());
    }

    fn bytes(&mut self, b: &[u8]) {
        self.u32(len(b.len()));
        self.0.extend_from_slice(b);
    }

    fn opt_u32(&mut self, n: Option<u32>) {
        match n {
            None => self.byte(0),
            Some(n) => {
                self.byte(1);
                self.u32(n);
            }
        }
    }

    fn run(&mut self, run: RunId) {
        self.bytes(run.to_string().as_bytes());
    }

    fn time(&mut self, t: Timestamp) {
        self.0.extend_from_slice(&t.as_millis().to_le_bytes());
    }
}

/// Reads one entry from the front of `bytes` and moves past it; `None` if they do not begin with
/// an entry this format writes, kind 0 included. It reads back as a [`Stored`], which is folded,
/// never decided again.
pub fn decode(bytes: &mut &[u8]) -> Option<Stored> {
    let mut d = Dec(bytes);
    let entry = match d.byte()? {
        FUNCTION_PUT => {
            let name = d.text()?;
            let code = d.text()?;
            let count = d.u32()?;
            let mut triggers = Vec::new();
            for _ in 0..count {
                triggers.push(d.text()?);
            }
            Entry::FunctionPut(Definition {
                name,
                code,
                triggers,
                max_attempts: d.opt_u32()?,
                first_delay_seconds: d.opt_u32()?,
                max_delay_seconds: d.opt_u32()?,
            })
        }
        RUN_CREATED => Entry::RunCreated {
            run: d.run()?,
            sequence: u64::from_le_bytes(d.take()?),
            function: d.text()?,
            code: d.text()?,
            input: d.bytes()?.to_vec(),
        },
        CLAIMED => Entry::Claimed {
            run: d.run()?,
            execution: d.u32()?,
            lease_until: d.time()?,
            lease_seconds: d.u32()?,
        },
        LEASE_RENEWED => Entry::LeaseRenewed {
            run: d.run()?,
            lease_until: d.time()?,
        },
        LEASE_LOST => Entry::LeaseLost {
            run: d.run()?,
            lost_leases: d.u32()?,
        },
        RELEASED => Entry::Released { run: d.run()? },
        STEP_BEGAN => Entry::StepBegan {
            run: d.run()?,
            position: d.u32()?,
            name: d.text()?,
            once: match d.byte()? {
                0 => false,
                1 => true,
                _ => return None,
            },
            input: match d.byte()? {
                0 => None,
                1 => Some(d.bytes()?.to_vec()),
                _ => return None,
            },
            execution: d.u32()?,
        },
        STEP_COMPLETED => Entry::StepCompleted {
            run: d.run()?,
            position: d.u32()?,
            output: d.bytes()?.to_vec(),
        },
        RUN_COMPLETED => Entry::RunCompleted(RunCompleted {
            run: d.run()?,
            output: d.bytes()?.to_vec(),
        }),
        RUN_FAILED => Entry::RunFailed(RunFailed {
            run: d.run()?,
            error: RunError {
                code: d.text()?,
                message: d.text()?,
            },
        }),
        _ => return None,
    };
    Some(Stored(entry))
}

struct Dec<'a, 'b>(&'b mut &'a [u8]);

impl<'a> Dec<'a, '_> {
    fn take<const N: usize>(&mut self) -> Option<[u8; N]> {
        let (head, rest) = self.0.split_first_chunk::<N>()?;
        *self.0 = rest;
        Some(*head)
    }

    fn byte(&mut self) -> Option<u8> {
        self.take::<1>().map(|[b]| b)
    }

    fn u32(&mut self) -> Option<u32> {
        self.take().map(u32::from_le_bytes)
    }

    fn bytes(&mut self) -> Option<&'a [u8]> {
        let len = usize::try_from(self.u32()?).ok()?;
        let (head, rest) = self.0.split_at_checked(len)?;
        *self.0 = rest;
        Some(head)
    }

    fn text(&mut self) -> Option<String> {
        String::from_utf8(self.bytes()?.to_vec()).ok()
    }

    fn opt_u32(&mut self) -> Option<Option<u32>> {
        match self.byte()? {
            0 => Some(None),
            1 => self.u32().map(Some),
            _ => None,
        }
    }

    fn run(&mut self) -> Option<RunId> {
        std::str::from_utf8(self.bytes()?).ok()?.parse().ok()
    }

    fn time(&mut self) -> Option<Timestamp> {
        self.take()
            .map(|b| Timestamp::from_millis(i64::from_le_bytes(b)))
    }
}
