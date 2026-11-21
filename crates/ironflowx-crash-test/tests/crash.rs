//! The parent side of the crash test (#6, invariants.md "Crash test"). It sees the child only
//! through the pipe, then checks the directory through a reopened ledger and, read-only, the
//! read store, for the sequences and applied position no public interface exposes.

use std::io::{BufRead, BufReader, Write};
use std::num::NonZeroU32;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use ironflowx_cell::{FaultPoint, Ledger, LocalOperations, ManualClock};
use ironflowx_core::{EnvRef, Timestamp};
use ironflowx_proto::{Call, Event, GetEventRequest, ListEventsRequest, Operations};

/// How long the parent waits for one line from the child before failing, so a stalled child
/// fails the test instead of hanging it.
const ANSWER_DEADLINE: Duration = Duration::from_secs(30);

/// The child process, stopped at its `crossing`-th pass of `point`. Dropping it kills the
/// child, so a failed assertion leaves no process parked at a point.
struct Driver {
    child: Child,
    stdin: ChildStdin,
    /// The child's stdout lines, read on their own thread so each read can time out.
    lines: mpsc::Receiver<std::io::Result<String>>,
}

/// What the child answered to one request.
#[derive(Debug, PartialEq, Eq)]
enum Answer {
    Acked(String),
    Failed(String),
    /// The child stopped at the named point and waits there to be killed.
    AtPoint(String),
}

impl Driver {
    fn spawn(dir: &Path, env: EnvRef, point: &str, crossing: u64) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_crash-child"))
            .arg(dir)
            .arg(env.to_string())
            .arg(point)
            .arg(crossing.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("the child starts");
        let stdin = child.stdin.take().expect("piped");
        let stdout = BufReader::new(child.stdout.take().expect("piped"));
        let (send, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in stdout.lines() {
                if send.send(line).is_err() {
                    break;
                }
            }
        });
        let driver = Self {
            child,
            stdin,
            lines,
        };
        assert_eq!(driver.line("ready"), "ready", "the child opens its ledger");
        driver
    }

    fn line(&self, expecting: &str) -> String {
        match self.lines.recv_timeout(ANSWER_DEADLINE) {
            Ok(line) => line.expect("the pipe reads"),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                panic!("no answer from the child within {ANSWER_DEADLINE:?}, expecting {expecting}")
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!("the child exited, expecting {expecting}")
            }
        }
    }

    fn emit(&mut self, name: &str) -> Answer {
        writeln!(self.stdin, "emit {name}").expect("the pipe writes");
        let line = self.line(&format!("an answer to emit {name}"));
        match line.split_once(' ') {
            Some(("ack", id)) => Answer::Acked(id.to_owned()),
            Some(("err", kind)) => Answer::Failed(kind.to_owned()),
            Some(("at", point)) => Answer::AtPoint(point.to_owned()),
            _ => panic!("the child answered {line:?}"),
        }
    }

    /// SIGKILL: no destructor, flush or close runs in the child.
    fn kill(mut self) {
        self.child.kill().expect("the child is killed");
        let status = self.child.wait().expect("the child is reaped");
        assert_eq!(
            status.signal(),
            Some(9),
            "the child died of the parent's SIGKILL, not on its own: {status}"
        );
    }
}

impl Drop for Driver {
    fn drop(&mut self) {
        // Already reaped after `kill`, where this is a no-op.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// What two reopens must agree on.
#[derive(Debug, PartialEq, Eq)]
struct Snapshot {
    /// Every event a client lists, oldest first, with every field.
    events: Vec<Event>,
    /// Each event's sequence and ID in the read store, in sequence order.
    sequences: Vec<(u64, String)>,
    applied: u64,
}

fn clock() -> ManualClock {
    ManualClock::new(Timestamp::from_millis(1_700_000_000_000))
}

/// Reopens the directory, checks it holds exactly the acknowledged emits, in order, closes,
/// then reads the read store.
fn reopen(dir: &Path, env: EnvRef, acked: &[(String, String)]) -> Snapshot {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("a runtime");
    let events = runtime.block_on(async {
        let ledger = Ledger::open(dir, env, clock())
            .await
            .expect("the ledger reopens after the kill");
        let ops = LocalOperations::new(&ledger);
        let cx = Call::anonymous();
        for (name, id) in acked {
            let event = ops
                .get_event(
                    &cx,
                    GetEventRequest {
                        event_id: id.clone(),
                    },
                )
                .await
                .unwrap_or_else(|e| panic!("acknowledged {name} ({id}) reads back: {e:?}"));
            assert_eq!(&event.name, name);
        }
        let page = ops
            .list_events(
                &cx,
                ListEventsRequest {
                    limit: NonZeroU32::new(200),
                    cursor: None,
                },
            )
            .await
            .expect("events list");
        assert!(page.next.is_none(), "one page holds every event");
        ledger.close().await;
        page.events.into_iter().rev().collect::<Vec<_>>()
    });
    let expected: Vec<&String> = acked.iter().map(|(n, _)| n).collect();
    assert_eq!(
        events.iter().map(|e| &e.name).collect::<Vec<_>>(),
        expected,
        "every acknowledged emit, in order, and nothing of the unanswered one"
    );
    let (sequences, applied) = read_store(dir);
    Snapshot {
        events,
        sequences,
        applied,
    }
}

/// Each event's sequence and ID, and the applied position, read-only.
fn read_store(dir: &Path) -> (Vec<(u64, String)>, u64) {
    let conn = rusqlite::Connection::open_with_flags(
        dir.join("read.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .expect("the read store opens read-only");
    let mut q = conn
        .prepare("SELECT sequence, id FROM events ORDER BY sequence")
        .expect("the events query prepares");
    let events = q
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .expect("events read")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("every row reads");
    let applied = conn
        .query_row("SELECT applied FROM meta", [], |r| r.get(0))
        .expect("the applied position reads");
    let sequences: Vec<u64> = events.iter().map(|e: &(u64, String)| e.0).collect();
    let expected: Vec<u64> = (1..=sequences.len() as u64).collect();
    assert_eq!(
        sequences, expected,
        "event sequences have no gap and no repeat"
    );
    (events, applied)
}

#[test]
fn killed_before_the_append() {
    // Before the append, the unanswered emit cannot have reached the log: it must leave
    // nothing, the strict half of "nothing or a whole record".
    let point = FaultPoint::BeforeAppend.name();
    let dir = tempfile::tempdir().expect("a temporary directory");
    let env = ironflowx_cell::new_env_ref();
    let mut child = Driver::spawn(dir.path(), env, point, 6);
    let mut acked = Vec::new();
    let mut stopped = false;
    for i in 1..=6 {
        let name = format!("crash.e{i}");
        match child.emit(&name) {
            Answer::Acked(id) => acked.push((name, id)),
            // The child names only the point it was given, so the name proves nothing;
            // that it stopped on the sixth emit, after five acks, is the check.
            Answer::AtPoint(_) => {
                stopped = true;
                break;
            }
            Answer::Failed(kind) => panic!("{name} failed: {kind}"),
        }
    }
    assert!(stopped, "the child stopped at its sixth append");
    assert_eq!(acked.len(), 5);
    child.kill();

    let first = reopen(dir.path(), env, &acked);
    let second = reopen(dir.path(), env, &acked);
    assert_eq!(first, second, "two reopens agree");
}
