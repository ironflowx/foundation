//! The parent side of the crash test (#6, invariants.md "Crash test"). It sees the child only
//! through the pipe, then checks the directory through a reopened ledger and, read-only, the
//! read store, for the sequences and applied position no public interface exposes.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Write};
use std::num::NonZeroU32;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use ironflowx_cell::{FaultPoint, Ledger, LocalOperations, ManualClock};
use ironflowx_core::{EnvRef, Timestamp};
use ironflowx_proto::{Call, EmitRequest, Event, GetEventRequest, ListEventsRequest, Operations};

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
    Line(String),
    /// The child stopped at its point and waits there to be killed.
    AtPoint,
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

    /// Sends one request line; the child names only the point it was given, so `at` carries
    /// nothing to check.
    fn request(&mut self, line: &str) -> Answer {
        writeln!(self.stdin, "{line}").expect("the pipe writes");
        let answer = self.line(&format!("an answer to {line:?}"));
        if answer.starts_with("at ") {
            Answer::AtPoint
        } else {
            Answer::Line(answer)
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

/// A name event rules refuse: it has an empty segment.
const REFUSED_NAME: &str = "crash..refused";

/// One item of a request, as a client builds it.
struct Item {
    name: &'static str,
    key: Option<&'static str>,
    /// Unique per item; the child puts it in the metadata.
    marker: String,
}

impl Item {
    fn line(&self) -> String {
        format!("{},{},{}", self.name, self.key.unwrap_or("-"), self.marker)
    }
}

enum Request {
    Emit(Item),
    Batch(Vec<Item>),
    /// A page of this many events, from the newest.
    List(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expect {
    New,
    Dup,
    Refused,
}

/// One item the parent sent, what it expected, and the event ID it got back, if any.
struct SentItem {
    request: String,
    marker: String,
    expect: Expect,
    acked: Option<String>,
}

/// Everything a client saw before the kill.
#[derive(Default)]
struct Seen {
    items: Vec<SentItem>,
    /// Each acknowledged key, the request that bound it, and its event ID.
    keys: BTreeMap<String, (String, String)>,
    /// The last list: its request, limit, the event IDs it returned and its cursor.
    cursor: Option<(String, u32, Vec<String>, String)>,
    /// The request the kill left without an answer.
    unanswered: Vec<String>,
}

impl Seen {
    /// Each item's outcome, from the keys acknowledged so far and those earlier in the request.
    fn expect(&self, items: &[Item]) -> Vec<Expect> {
        let mut earlier = BTreeSet::new();
        items
            .iter()
            .map(|i| match i.key {
                _ if i.name == REFUSED_NAME => Expect::Refused,
                Some(k) if self.keys.contains_key(k) || !earlier.insert(k) => Expect::Dup,
                _ => Expect::New,
            })
            .collect()
    }

    /// Checks one item's answer, `new:<id>`, `dup:<id>` or `err:<kind>`, and records it.
    fn answer(&mut self, request: &str, item: &Item, expect: Expect, answer: &str) {
        let acked = match (expect, answer.split_once(':')) {
            (Expect::New, Some(("new", id))) => {
                if let Some(k) = item.key {
                    self.keys
                        .insert(k.to_owned(), (request.to_owned(), id.to_owned()));
                }
                Some(id.to_owned())
            }
            (Expect::Dup, Some(("dup", id))) => {
                let (_, original) = &self.keys[item.key.expect("a duplicate has a key")];
                assert_eq!(
                    id, original,
                    "{request}: a duplicate answers the original ID"
                );
                None
            }
            (Expect::Refused, Some(("err", "Invalid"))) => None,
            _ => panic!(
                "{request}: {} expected {expect:?}, got {answer}",
                item.marker
            ),
        };
        self.items.push(SentItem {
            request: request.to_owned(),
            marker: item.marker.clone(),
            expect,
            acked,
        });
    }

    fn unanswered(&mut self, request: &str, items: &[Item], expect: &[Expect]) {
        self.unanswered.push(request.to_owned());
        for (item, &expect) in items.iter().zip(expect) {
            self.items.push(SentItem {
                request: request.to_owned(),
                marker: item.marker.clone(),
                expect,
                acked: None,
            });
        }
    }
}

/// What a real client sends: keyed and unkeyed emits, a duplicate and a refused emit, mixed
/// batches with new items, stored and in-batch duplicates and refusals, and a list between
/// them. The last request is a mixed batch, the fifth to append.
fn workload() -> Vec<Request> {
    let mut n = 0;
    let mut item = |name, key| {
        n += 1;
        Item {
            name,
            key,
            marker: format!("m{n}"),
        }
    };
    vec![
        Request::Emit(item("crash.keyed", Some("k1"))),
        Request::Emit(item("crash.unkeyed", None)),
        Request::Emit(item("crash.again", Some("k1"))),
        Request::Emit(item(REFUSED_NAME, None)),
        Request::Batch(vec![
            item("crash.keyed", Some("k2")),
            item("crash.unkeyed", None),
            item("crash.again", Some("k1")),
            item(REFUSED_NAME, Some("k9")),
            item("crash.again", Some("k2")),
        ]),
        Request::List(3),
        Request::Emit(item("crash.keyed", Some("k3"))),
        Request::Batch(vec![
            item("crash.keyed", Some("k4")),
            item("crash.again", Some("k2")),
            item(REFUSED_NAME, None),
            item("crash.unkeyed", None),
            item("crash.again", Some("k4")),
            item("crash.again", Some("k3")),
        ]),
    ]
}

/// The number of requests in `workload` that append a record, so the last one stops at the
/// "before the append" point.
const APPENDS: u64 = 5;

/// Runs the workload until the child stops before its last append, checking every answer,
/// then kills the child.
fn run(dir: &Path, env: EnvRef) -> Seen {
    let mut child = Driver::spawn(dir, env, FaultPoint::BeforeAppend.name(), APPENDS);
    let mut seen = Seen::default();
    let requests = workload();
    let count = requests.len();
    for (i, request) in requests.into_iter().enumerate() {
        let label = format!("request {}", i + 1);
        let (line, items, batch) = match request {
            Request::Emit(item) => (format!("emit {}", item.line()), vec![item], false),
            Request::Batch(items) => {
                let lines: Vec<_> = items.iter().map(Item::line).collect();
                (format!("batch {}", lines.join(" ")), items, true)
            }
            Request::List(limit) => {
                let Answer::Line(page) = child.request(&format!("list {limit} -")) else {
                    panic!("{label}: a list stopped at the point");
                };
                let Some(page) = page.strip_prefix("page ") else {
                    panic!("{label}: the list answered {page}");
                };
                let mut words = page.split(' ');
                let next = words.next().expect("a cursor").to_owned();
                assert_ne!(next, "-", "{label}: the page is not the last");
                let ids: Vec<String> = words.map(str::to_owned).collect();
                assert!(!ids.is_empty(), "{label}: the page holds an event");
                seen.cursor = Some((label, limit, ids, next));
                continue;
            }
        };
        let expect = seen.expect(&items);
        let Answer::Line(answer) = child.request(&line) else {
            assert_eq!(
                i + 1,
                count,
                "{label}: only the last request stops at the point"
            );
            seen.unanswered(&label, &items, &expect);
            break;
        };
        let results: Vec<&str> = answer.split(' ').collect();
        if batch && items.len() > 1 && results.len() == 1 {
            panic!("{label}: the whole batch failed: {answer}");
        }
        assert_eq!(results.len(), items.len(), "{label}: one result per item");
        for ((item, expect), result) in items.iter().zip(expect).zip(&results) {
            seen.answer(&label, item, expect, result);
        }
    }
    assert_eq!(
        seen.unanswered.len(),
        1,
        "the child stopped at its last append"
    );
    child.kill();
    seen
}

/// What two reopens must agree on.
#[derive(Debug, PartialEq, Eq)]
struct Snapshot {
    /// Every event a client lists, oldest first, with every field, keys included.
    events: Vec<Event>,
    /// Each event's sequence and ID in the read store, in sequence order.
    sequences: Vec<(u64, String)>,
    applied: u64,
}

fn clock() -> ManualClock {
    ManualClock::new(Timestamp::from_millis(1_700_000_000_000))
}

fn marker(event: &Event) -> Option<&str> {
    std::str::from_utf8(event.metadata.as_deref()?)
        .ok()?
        .strip_prefix(r#"{"marker":""#)?
        .strip_suffix(r#""}"#)
}

/// Reopens the directory, checks every promise a client saw before the kill, closes, then
/// reads the read store. A broken promise is an error naming its request.
fn reopen(dir: &Path, env: EnvRef, seen: &Seen) -> Result<Snapshot, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("a runtime");
    let events = runtime.block_on(async {
        let ledger = Ledger::open(dir, env, clock())
            .await
            .expect("the ledger reopens after the kill");
        let checked = check(&LocalOperations::new(&ledger), seen).await;
        ledger.close().await;
        checked
    })?;
    let (sequences, applied) = read_store(dir);
    Ok(Snapshot {
        events,
        sequences,
        applied,
    })
}

/// Every client-visible promise; returns every event, oldest first.
async fn check(ops: &LocalOperations<'_>, seen: &Seen) -> Result<Vec<Event>, String> {
    let cx = Call::anonymous();
    for item in &seen.items {
        let Some(id) = &item.acked else { continue };
        let event = ops
            .get_event(
                &cx,
                GetEventRequest {
                    event_id: id.clone(),
                },
            )
            .await
            .map_err(|e| {
                format!(
                    "{}: acknowledged {id} does not read back: {e}",
                    item.request
                )
            })?;
        if marker(&event) != Some(&item.marker) {
            return Err(format!("{}: {id} reads back another event", item.request));
        }
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
        .map_err(|e| format!("events list: {e}"))?;
    assert!(page.next.is_none(), "one page holds every event");
    let newest_first: Vec<String> = page.events.iter().map(|e| e.event_id.to_string()).collect();
    // Oldest first, an unmarked event as `None`, so a stray or doubled event shows.
    let listed: Vec<Option<&str>> = page.events.iter().rev().map(marker).collect();
    let stored: BTreeSet<&str> = listed.iter().flatten().copied().collect();
    let sent: BTreeSet<&str> = seen.items.iter().map(|i| i.marker.as_str()).collect();
    if let Some(stray) = listed.iter().find(|m| m.is_none_or(|m| !sent.contains(m))) {
        return Err(format!(
            "an event with marker {stray:?} that no request sent"
        ));
    }
    for item in &seen.items {
        if item.expect != Expect::New && stored.contains(item.marker.as_str()) {
            return Err(format!(
                "{}: {} was {:?} yet stored an event",
                item.request, item.marker, item.expect
            ));
        }
    }
    // Before the append nothing can be present; later kill points (#6) make this branch bite.
    for request in &seen.unanswered {
        let new: Vec<_> = seen
            .items
            .iter()
            .filter(|i| &i.request == request && i.expect == Expect::New)
            .collect();
        let present = new
            .iter()
            .filter(|i| stored.contains(i.marker.as_str()))
            .count();
        if present != 0 && present != new.len() {
            return Err(format!(
                "{request}: {present} of its {} new events are present",
                new.len()
            ));
        }
    }

    // Exactly the acknowledged new events, once each, in acknowledgment order, then nothing or
    // all of the unanswered request's new events.
    let acked: Vec<&SentItem> = seen.items.iter().filter(|i| i.acked.is_some()).collect();
    for (n, item) in acked.iter().enumerate() {
        if listed.get(n) != Some(&Some(item.marker.as_str())) {
            return Err(format!(
                "{}: acknowledged {} is not event {} of {listed:?}",
                item.request,
                item.marker,
                n + 1
            ));
        }
    }
    let rest = listed.get(acked.len()..).unwrap_or_default();
    let unanswered: Vec<Option<&str>> = seen
        .items
        .iter()
        .filter(|i| seen.unanswered.contains(&i.request) && i.expect == Expect::New)
        .map(|i| Some(i.marker.as_str()))
        .collect();
    if !rest.is_empty() && rest != unanswered {
        return Err(format!(
            "{:?}: after the acknowledged events come {rest:?}, not nothing or {unanswered:?}",
            seen.unanswered
        ));
    }

    if let Some((request, limit, ids, cursor)) = &seen.cursor {
        let at = newest_first
            .iter()
            .position(|id| Some(id) == ids.last())
            .ok_or_else(|| format!("{request}: the last listed event is gone"))?;
        let expected: Vec<&String> = newest_first[at + 1..]
            .iter()
            .take(*limit as usize)
            .collect();
        let next = ops
            .list_events(
                &cx,
                ListEventsRequest {
                    limit: NonZeroU32::new(*limit),
                    cursor: Some(cursor.clone()),
                },
            )
            .await
            .map_err(|e| format!("{request}: its cursor no longer pages: {e}"))?;
        let got: Vec<String> = next.events.iter().map(|e| e.event_id.to_string()).collect();
        if got.iter().collect::<Vec<_>>() != expected {
            return Err(format!(
                "{request}: its cursor pages {got:?}, not from the same place {expected:?}"
            ));
        }
    }

    // Last, after every read: a lost key makes this emit store a new event.
    for (key, (request, id)) in &seen.keys {
        let again = EmitRequest {
            name: "crash.again".into(),
            data: None,
            metadata: None,
            version: None,
            idempotency_key: Some(key.clone()),
        };
        match ops.emit(&cx, again).await {
            Ok(r) if r.duplicate && r.event_id.to_string() == *id => {}
            other => {
                return Err(format!(
                    "{request}: acknowledged key {key} answers {other:?}, not its original {id}"
                ));
            }
        }
    }
    Ok(page.events.into_iter().rev().collect())
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
    // Before the append, the unanswered batch cannot have reached the log: its new events must
    // all be absent, the strict half of "all or nothing".
    let dir = tempfile::tempdir().expect("a temporary directory");
    let env = ironflowx_cell::new_env_ref();
    let seen = run(dir.path(), env);

    let first = reopen(dir.path(), env, &seen).unwrap_or_else(|e| panic!("{e}"));
    let unanswered = &seen.unanswered[0];
    assert!(
        !seen.items.iter().any(|i| &i.request == unanswered
            && first.events.iter().any(|e| marker(e) == Some(&i.marker))),
        "{unanswered}: nothing of it is stored before the append"
    );
    let second = reopen(dir.path(), env, &seen).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(first, second, "two reopens agree");
}

#[test]
fn a_broken_expectation_names_its_request() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let env = ironflowx_cell::new_env_ref();
    let mut seen = run(dir.path(), env);
    // A key the client saw acknowledged that the ledger never stored.
    let (request, id) = seen.keys["k1"].clone();
    seen.keys.insert("k-missing".into(), (request.clone(), id));

    let err = reopen(dir.path(), env, &seen).expect_err("a missing acknowledged key fails");
    assert!(
        err.starts_with(&format!("{request}: acknowledged key k-missing")),
        "the failure names the request: {err}"
    );
}
