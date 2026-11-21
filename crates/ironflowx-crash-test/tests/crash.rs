//! The parent side of the crash test (#6, invariants.md "Crash test"). It sees the child only
//! through the pipe, then checks the directory through a reopened ledger and, read-only, the
//! read store, for the sequences and applied position no public interface exposes.

use std::io::{BufRead, BufReader, Lines, Write};
use std::num::NonZeroU32;
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use ironflowx_cell::{FaultPoint, Ledger, LocalOperations, ManualClock};
use ironflowx_core::{EnvRef, Timestamp};
use ironflowx_proto::{Call, GetEventRequest, ListEventsRequest, Operations};

/// The child process, stopped at its `crossing`-th pass of `point`.
struct Driver {
    child: Child,
    stdin: ChildStdin,
    stdout: Lines<BufReader<ChildStdout>>,
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
        let stdout = BufReader::new(child.stdout.take().expect("piped")).lines();
        let mut driver = Self {
            child,
            stdin,
            stdout,
        };
        assert_eq!(driver.line(), "ready", "the child opens its ledger");
        driver
    }

    fn line(&mut self) -> String {
        self.stdout
            .next()
            .expect("the child answers before exiting")
            .expect("the pipe reads")
    }

    fn emit(&mut self, name: &str) -> Answer {
        writeln!(self.stdin, "emit {name}").expect("the pipe writes");
        let line = self.line();
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
        self.child.wait().expect("the child is reaped");
    }
}

/// What two reopens must agree on.
#[derive(Debug, PartialEq, Eq)]
struct Snapshot {
    /// Each event's sequence, ID, name and time, in sequence order.
    events: Vec<(u64, String, String, i64)>,
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
    let names = runtime.block_on(async {
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
        page.events
            .into_iter()
            .rev()
            .map(|e| e.name)
            .collect::<Vec<_>>()
    });
    let expected: Vec<&String> = acked.iter().map(|(n, _)| n).collect();
    assert_eq!(
        names.iter().collect::<Vec<_>>(),
        expected,
        "every acknowledged emit, in order, and nothing of the unanswered one"
    );
    read_store(dir)
}

fn read_store(dir: &Path) -> Snapshot {
    let conn = rusqlite::Connection::open_with_flags(
        dir.join("read.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .expect("the read store opens read-only");
    let mut q = conn
        .prepare("SELECT sequence, id, name, time FROM events ORDER BY sequence")
        .expect("the events query prepares");
    let events = q
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .expect("events read")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("every row reads");
    let applied = conn
        .query_row("SELECT applied FROM meta", [], |r| r.get(0))
        .expect("the applied position reads");
    let sequences: Vec<u64> = events.iter().map(|e: &(u64, _, _, _)| e.0).collect();
    let expected: Vec<u64> = (1..=sequences.len() as u64).collect();
    assert_eq!(
        sequences, expected,
        "event sequences have no gap and no repeat"
    );
    Snapshot { events, applied }
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
            Answer::AtPoint(at) => {
                assert_eq!(at, point);
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
