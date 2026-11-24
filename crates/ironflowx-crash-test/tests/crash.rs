//! The parent side of the crash test (#6, invariants.md "Crash test"). It sees the child only
//! through its pipe and its socket, then checks the directory through a reopened engine and,
//! read-only, the read store, for the sequences and applied position no public interface exposes.
//! Kills while the ledger works are here; those inside a bootstrap or a seed change are in
//! `crash/opens.rs`, and those around a key change or an expiry's refusal in `crash/keys.rs`.

#[path = "crash/keys.rs"]
mod keys;
#[path = "crash/opens.rs"]
mod opens;

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::num::NonZeroU32;
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, MutexGuard, PoisonError, mpsc};
use std::time::Duration;

use ironflowx_authz::parse;
use ironflowx_cell::ManualClock;
use ironflowx_core::{Clock, EnvRef, FaultPoint, KeyId, Secret, Timestamp};
use ironflowx_operations::Engine;
use ironflowx_proto::{
    Call, EmitRequest, Event, GetEventRequest, ListEnvironmentsRequest, ListEventsRequest,
    Operations,
};

/// How long the parent waits for one line from the child before failing, so a stalled child
/// fails the test instead of hanging it.
const ANSWER_DEADLINE: Duration = Duration::from_secs(30);

/// The seed each run's first open takes, so the parent always knows an admin key.
const SEED: &str =
    "ixk_019a9f3e-7c2b-7d41-8a3e-200000000001_AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE";

/// The ID in a key's text.
fn key_id(key: &str) -> KeyId {
    let Ok(parts) = parse(&Secret::new(key.to_owned())) else {
        panic!("the parent holds a malformed key");
    };
    parts.id
}

/// The emitter of events the key whose text is `key` emits.
fn emitter(key: &str) -> String {
    format!("key:{}", key_id(key))
}

/// Held by each test: a child process shares the lock files open in this one until it execs, so a
/// test that closes an engine and opens it again beside a spawn could find it locked.
fn alone() -> MutexGuard<'static, ()> {
    static ALONE: Mutex<()> = Mutex::new(());
    ALONE.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The child process, stopped at its `crossing`-th pass of `point`. Dropping it kills the
/// child, so a failed assertion leaves no process parked at a point.
struct Driver {
    child: Child,
    /// The child's stdin, and the channel it tells identities and secrets on.
    socket: UnixStream,
    /// The child's stdout lines, read on their own thread so each read can time out.
    lines: mpsc::Receiver<std::io::Result<String>>,
}

/// A key the child's engine was about to store, told before any point its storing reaches. Not
/// `Debug`: a failed assertion must not print its secret.
struct Told {
    env: EnvRef,
    key: KeyId,
    /// The key's text, unless it is the seed.
    secret: Option<String>,
}

/// What the child bootstrapped with `SEED`: its engine's environment and the second key it made.
struct Home {
    root: PathBuf,
    env: EnvRef,
    second: KeyId,
}

impl Home {
    /// The environment's ledger directory.
    fn ledger(&self) -> PathBuf {
        self.root.join("ledgers").join(self.env.id().to_string())
    }

    fn call(&self) -> Call {
        Call::with_key(Secret::new(SEED.to_owned()))
    }

    /// Opens the engine again without a seed: its bootstrap is done, so no key is minted, and
    /// `SEED` still reaches the environment the child reported, its incarnation included.
    async fn open(&self, clock: ManualClock) -> Engine {
        let opened = Engine::open(&self.root, clock, None)
            .await
            .expect("the engine reopens after the kill");
        assert!(opened.minted.is_none(), "a reopen mints no key");
        assert!(
            !opened.lost_secret,
            "a seeded bootstrap has no secret to lose"
        );
        assert_eq!(opened.seed, None, "a reopen without a seed applies none");
        let envs = opened
            .engine
            .list_environments(&self.call(), ListEnvironmentsRequest {})
            .await
            .expect("the seed still authenticates");
        let envs: Vec<EnvRef> = envs.iter().map(|e| e.env).collect();
        assert_eq!(
            envs,
            [self.env],
            "a reopen serves the environment the child reported"
        );
        opened.engine
    }
}

impl Driver {
    /// The child, its engine open on the fresh `root`, and what it bootstrapped.
    fn spawn(root: &Path, point: &str, crossing: u64) -> (Self, Home) {
        let driver = Self::start(root, point, crossing, Some(SEED), None);
        assert_eq!(driver.line("ready"), "ready", "the child opens its engine");
        let line = driver.line("the environment and the second key");
        let (env, second) = line.split_once(' ').expect("<env-ref> <key-id>");
        let home = Home {
            root: root.to_owned(),
            env: env.parse().expect("an environment reference"),
            second: second.parse().expect("a key ID"),
        };
        (driver, home)
    }

    /// The child, before its engine has opened with `seed`; obstructed, it is made to fail.
    fn start(
        root: &Path,
        point: &str,
        crossing: u64,
        seed: Option<&str>,
        obstruct: Option<&str>,
    ) -> Self {
        let (mut socket, theirs) = UnixStream::pair().expect("a socket pair");
        let mut child = Command::new(env!("CARGO_BIN_EXE_crash-child"))
            .arg(root)
            .arg(point)
            .arg(crossing.to_string())
            .args(obstruct)
            .stdin(OwnedFd::from(theirs))
            .stdout(Stdio::piped())
            .spawn()
            .expect("the child starts");
        writeln!(socket, "{}", seed.unwrap_or("-")).expect("the socket writes");
        let stdout = BufReader::new(child.stdout.take().expect("piped"));
        let (send, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in stdout.lines() {
                if send.send(line).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            socket,
            lines,
        }
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

    fn send(&mut self, line: &str) {
        writeln!(self.socket, "{line}").expect("the socket writes");
    }

    /// The answer to a request that must not stop at the point.
    fn answer(&self, request: &str) -> String {
        let line = self.line(&format!("an answer to {request}"));
        assert!(
            !line.starts_with("at "),
            "{request}: the child stopped at the point too early"
        );
        line
    }

    /// The answer to the request that stops at `point`, if the child sends one before it
    /// stops: only at a point after the acknowledgment (after-ack, mid-fold) does it, and the
    /// two lines come in either order.
    /// Before it, a `probe` proves no answer is on its way.
    fn stop(&mut self, request: &str, point: FaultPoint) -> Option<String> {
        let lines = if matches!(point, FaultPoint::AfterAck | FaultPoint::MidFold) {
            2
        } else {
            1
        };
        let mut answer = None;
        let mut stopped = false;
        for _ in 0..lines {
            let line = self.line(&format!("{request} to stop at {}", point.name()));
            if line == format!("at {}", point.name()) {
                stopped = true;
            } else {
                answer = Some(line);
            }
        }
        assert!(
            stopped,
            "{request}: the child answered without stopping at {}",
            point.name()
        );
        if answer.is_none() {
            self.send("probe");
            let line = self.line(&format!("{request} to be pending"));
            assert_eq!(
                line,
                "pending",
                "{request}: answered though stopped before the acknowledgment, at {}",
                point.name()
            );
        }
        answer
    }

    /// SIGKILL: no destructor, flush or close runs in the child. What it had told by then.
    fn kill(mut self) -> Vec<Told> {
        self.child.kill().expect("the child is killed");
        let status = self.child.wait().expect("the child is reaped");
        assert_eq!(
            status.signal(),
            Some(9),
            "the child died of the parent's SIGKILL, not on its own: {status}"
        );
        // The child is gone, so this reads to the end of what it wrote.
        let mut told = String::new();
        self.socket
            .read_to_string(&mut told)
            .expect("the socket reads");
        told.lines()
            .map(|line| {
                let [env, key, secret] = line.split(' ').collect::<Vec<_>>()[..] else {
                    panic!("an identity is <env-ref> <key-id> <secret>|-");
                };
                Told {
                    env: env.parse().expect("an environment reference"),
                    key: key.parse().expect("a key ID"),
                    secret: (secret != "-").then(|| secret.to_owned()),
                }
            })
            .collect()
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
    /// Steps the child's manual clock by this many milliseconds, backwards when negative.
    Clock(i64),
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
    /// The request the child stopped at the point on, answered or not.
    stopped: Option<String>,
    /// Each new item's marker and the time its event takes if stored: the clock the parent
    /// stepped, never behind the last record's (D7).
    times: BTreeMap<String, Timestamp>,
    /// Each new item's marker and the emitter its event names if stored: the key that sent it.
    emitters: BTreeMap<String, String>,
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
/// them, and clock steps both ways. The last request is a mixed batch, the fifth to append.
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
        Request::Clock(-5_000),
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
        Request::Clock(8_000),
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

/// The append of `workload` that is a single emit: its fourth.
const SINGLE_APPEND: u64 = 4;
/// The append of `workload` that is a mixed batch: its fifth.
const BATCH_APPEND: u64 = 5;

/// How many of `workload`'s requests append a record, if each is answered as expected.
fn appends(workload: &[Request]) -> u64 {
    let mut seen = Seen::default();
    let mut appends = 0;
    for request in workload {
        let items = match request {
            Request::Emit(item) => std::slice::from_ref(item),
            Request::Batch(items) => items,
            Request::List(_) | Request::Clock(_) => continue,
        };
        let expect = seen.expect(items);
        appends += u64::from(expect.contains(&Expect::New));
        for (item, expect) in items.iter().zip(expect) {
            if let (Expect::New, Some(k)) = (expect, item.key) {
                seen.keys.insert(k.to_owned(), Default::default());
            }
        }
    }
    appends
}

/// Runs `workload` until the child stops at `point` on its `append`-th append, checking every
/// answer, then kills the child.
fn run(root: &Path, point: FaultPoint, append: u64, workload: Vec<Request>) -> (Seen, Home) {
    let (mut child, home) = Driver::spawn(root, point.name(), append);
    let mut seen = Seen::default();
    let mut appends = 0;
    // The child's clock, as the parent stepped it, and the last record's time.
    let mut now = clock().now().as_millis();
    let mut last = None;
    for (i, request) in workload.into_iter().enumerate() {
        let label = format!("request {}", i + 1);
        // The two keys take turns.
        let (caller, emitter) = if i % 2 == 0 {
            (1, emitter(SEED))
        } else {
            (2, format!("key:{}", home.second))
        };
        let (line, items, batch) = match request {
            Request::Emit(item) => (format!("emit {caller} {}", item.line()), vec![item], false),
            Request::Batch(items) => {
                let lines: Vec<_> = items.iter().map(Item::line).collect();
                (format!("batch {caller} {}", lines.join(" ")), items, true)
            }
            Request::List(limit) => {
                child.send(&format!("list {limit} -"));
                let page = child.answer(&label);
                let Some(page) = page.strip_prefix("page ") else {
                    panic!("{label}: the list answered {page}");
                };
                let mut words = page.split(' ');
                let next = words.next().expect("a cursor").to_owned();
                // The last page has no cursor to keep.
                if next != "-" {
                    let ids: Vec<String> = words.map(str::to_owned).collect();
                    assert!(
                        !ids.is_empty(),
                        "{label}: a page with a cursor holds an event"
                    );
                    seen.cursor = Some((label, limit, ids, next));
                }
                continue;
            }
            Request::Clock(step) => {
                now += step;
                child.send(&format!("clock {step}"));
                assert_eq!(child.answer(&label), "ok", "{label}: the clock steps");
                continue;
            }
        };
        let expect = seen.expect(&items);
        appends += u64::from(expect.contains(&Expect::New));
        let stops = expect.contains(&Expect::New) && appends == append;
        if expect.contains(&Expect::New) {
            let time = last.map_or(now, |l: i64| l.max(now));
            last = Some(time);
            for (item, _) in items
                .iter()
                .zip(&expect)
                .filter(|(_, e)| **e == Expect::New)
            {
                seen.times
                    .insert(item.marker.clone(), Timestamp::from_millis(time));
                seen.emitters.insert(item.marker.clone(), emitter.clone());
            }
        }
        child.send(&line);
        let answer = if stops {
            seen.stopped = Some(label.clone());
            child.stop(&label, point)
        } else {
            Some(child.answer(&label))
        };
        let Some(answer) = answer else {
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
        if stops {
            break;
        }
    }
    assert!(
        seen.stopped.is_some(),
        "the workload has an append {append} to stop at"
    );
    let told = child.kill();
    let told: Vec<_> = told
        .iter()
        .map(|t| (t.env, t.key, t.secret.is_some()))
        .collect();
    assert_eq!(
        told,
        [
            (home.env, key_id(SEED), false),
            (home.env, home.second, true)
        ],
        "the child told its bootstrap key, the seed, and its second key"
    );
    (seen, home)
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

/// Reopens the engine, checks every promise a client saw before the kill, closes, then reads
/// the read store. A broken promise is an error naming its request.
fn reopen(home: &Home, seen: &Seen) -> Result<Snapshot, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("a runtime");
    let events = runtime.block_on(async {
        let engine = home.open(clock()).await;
        let checked = check(&engine, &home.call(), seen).await;
        engine.close().await;
        checked
    })?;
    let (sequences, applied) = read_store(home);
    Ok(Snapshot {
        events,
        sequences,
        applied,
    })
}

/// Every client-visible promise; returns every event, oldest first.
async fn check(engine: &Engine, cx: &Call, seen: &Seen) -> Result<Vec<Event>, String> {
    for item in &seen.items {
        let Some(id) = &item.acked else { continue };
        let event = engine
            .get_event(
                cx,
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
        if event.event_id.to_string() != *id || marker(&event) != Some(&item.marker) {
            return Err(format!("{}: {id} reads back another event", item.request));
        }
        let want = &seen.emitters[&item.marker];
        if event.emitted_by.to_string() != *want {
            return Err(format!(
                "{}: {id} reads back emitted by {}, not {want}",
                item.request, event.emitted_by
            ));
        }
    }
    let page = engine
        .list_events(
            cx,
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
    // D7: whichever way the clock stepped, each event takes the stepped clock, never behind the
    // event before it.
    for event in &page.events {
        let Some(want) = marker(event).and_then(|m| seen.times.get(m)) else {
            continue;
        };
        if event.time != *want {
            return Err(format!(
                "{} takes time {:?}, not {want:?}",
                marker(event).unwrap_or_default(),
                event.time
            ));
        }
    }
    // TEN-6: each event names the key that emitted it.
    for event in &page.events {
        let Some(want) = marker(event).and_then(|m| seen.emitters.get(m)) else {
            continue;
        };
        if event.emitted_by.to_string() != *want {
            return Err(format!(
                "{} is emitted by {}, not {want}",
                marker(event).unwrap_or_default(),
                event.emitted_by
            ));
        }
    }
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
    // Before the append none are present and after fsync all are; a batch split across records
    // would leave some, which this names (D9: a batch is one record).
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
        let next = engine
            .list_events(
                cx,
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
        match engine.emit(cx, again).await {
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
fn read_store(home: &Home) -> (Vec<(u64, String)>, u64) {
    let conn = rusqlite::Connection::open_with_flags(
        home.ledger().join("read.db"),
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

/// Kills the child at `point` on its `append`-th append of `workload`, then checks what the read
/// store held before reopen, which of the stopped request's new events reopen shows, and that
/// two reopens agree. Returns what the client saw, where, and what the reopens show.
fn kill_at(
    root: &Path,
    point: FaultPoint,
    append: u64,
    workload: Vec<Request>,
) -> (Seen, Home, Snapshot) {
    // During open needs a record for open to fold: stop after the acknowledgment first.
    let (seen, home) = if point == FaultPoint::DuringOpen {
        let ran = run(root, FaultPoint::AfterAck, append, workload);
        kill_during_open(root);
        ran
    } else {
        run(root, point, append, workload)
    };
    let stopped = seen.stopped.as_deref().expect("the child stopped");
    let name = point.name();

    // Every point is before the stopped request's fold or, mid-fold, inside it, which the kill
    // rolls back: the read store holds exactly the earlier requests' events until reopen folds
    // the rest, its rows and applied position moving together.
    let earlier: Vec<&String> = seen
        .items
        .iter()
        .filter(|i| i.request != stopped)
        .filter_map(|i| i.acked.as_ref())
        .collect();
    let (folded, applied) = read_store(&home);
    let folded: Vec<&String> = folded.iter().map(|(_, id)| id).collect();
    let mine = seen
        .items
        .iter()
        .filter(|i| i.request == stopped)
        .filter_map(|i| i.acked.as_ref());
    for id in mine {
        assert!(
            !folded.contains(&id),
            "{stopped}: killed at {name}, its {id} is folded before reopen"
        );
    }
    assert_eq!(
        folded, earlier,
        "killed at {name} on {stopped}: before reopen the read store holds every earlier \
         acknowledged event"
    );
    // The genesis is record 0 and each earlier append one more record.
    assert_eq!(
        applied, append,
        "killed at {name} on {stopped}: before reopen the applied position follows the earlier \
         records"
    );

    let first = reopen(&home, &seen).unwrap_or_else(|e| panic!("{e}"));
    let new: Vec<&SentItem> = seen
        .items
        .iter()
        .filter(|i| i.request == stopped && i.expect == Expect::New)
        .collect();
    let present = new
        .iter()
        .filter(|i| first.events.iter().any(|e| marker(e) == Some(&i.marker)))
        .count();
    // Before the append the record cannot reach the log; after fsync it is durable.
    let expected = if point == FaultPoint::BeforeAppend {
        0
    } else {
        new.len()
    };
    assert_eq!(
        present,
        expected,
        "{stopped}: killed at {name}, {present} of its {} new events are present after reopen",
        new.len()
    );
    assert_eq!(
        first.applied,
        append + u64::from(expected != 0),
        "killed at {name} on {stopped}: reopen folds every durable record"
    );
    let second = reopen(&home, &seen).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(first, second, "killed at {name}, two reopens agree");
    (seen, home, first)
}

/// Kills a reopen of `root` inside its fold of the record the last kill left unfolded, its rows
/// written and nothing committed.
fn kill_during_open(root: &Path) {
    let told = kill_inside_open(root, FaultPoint::DuringOpen, None, None);
    assert!(told.is_empty(), "a later open makes no key");
}

/// Kills the child inside its open of `root`, at its first pass of `point`.
fn kill_inside_open(
    root: &Path,
    point: FaultPoint,
    seed: Option<&str>,
    obstruct: Option<&str>,
) -> Vec<Told> {
    let name = point.name();
    let child = Driver::start(root, name, 1, seed, obstruct);
    let line = child.line(&format!("the open to stop at {name}"));
    assert_eq!(
        line,
        format!("at {name}"),
        "the open stops at {name}, before it is ready"
    );
    child.kill()
}

/// `kill_at` with the fixed workload, in a fresh directory.
fn kill_named(point: FaultPoint, append: u64) {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let (seen, ..) = kill_at(dir.path(), point, append, workload());
    // Four events precede its list of three, so the page has a cursor for `check` to page from.
    assert!(
        seen.cursor.is_some(),
        "the fixed workload's list is not the last page"
    );
}

#[test]
fn killed_before_the_append() {
    let _alone = alone();
    kill_named(FaultPoint::BeforeAppend, SINGLE_APPEND);
    kill_named(FaultPoint::BeforeAppend, BATCH_APPEND);
}

#[test]
fn killed_after_fsync() {
    let _alone = alone();
    kill_named(FaultPoint::AfterFsync, SINGLE_APPEND);
    kill_named(FaultPoint::AfterFsync, BATCH_APPEND);
}

#[test]
fn killed_after_the_acknowledgment() {
    let _alone = alone();
    kill_named(FaultPoint::AfterAck, SINGLE_APPEND);
    kill_named(FaultPoint::AfterAck, BATCH_APPEND);
}

#[test]
fn killed_mid_fold() {
    let _alone = alone();
    kill_named(FaultPoint::MidFold, SINGLE_APPEND);
    kill_named(FaultPoint::MidFold, BATCH_APPEND);
}

#[test]
fn killed_during_open() {
    let _alone = alone();
    kill_named(FaultPoint::DuringOpen, SINGLE_APPEND);
    kill_named(FaultPoint::DuringOpen, BATCH_APPEND);
}

#[test]
fn a_broken_expectation_names_its_request() {
    let _alone = alone();
    let dir = tempfile::tempdir().expect("a temporary directory");
    let (mut seen, home) = run(
        dir.path(),
        FaultPoint::BeforeAppend,
        BATCH_APPEND,
        workload(),
    );
    // A key the client saw acknowledged that the ledger never stored.
    let (request, id) = seen.keys["k1"].clone();
    seen.keys.insert("k-missing".into(), (request.clone(), id));

    let err = reopen(&home, &seen).expect_err("a missing acknowledged key fails");
    assert!(
        err.starts_with(&format!("{request}: acknowledged key k-missing")),
        "the failure names the request: {err}"
    );
}

/// Every named point the working ledger reaches, for the seeded kills to choose from; `opens` and
/// `keys` have the rest.
const POINTS: [FaultPoint; 5] = [
    FaultPoint::BeforeAppend,
    FaultPoint::AfterFsync,
    FaultPoint::AfterAck,
    FaultPoint::MidFold,
    FaultPoint::DuringOpen,
];

/// Fails to build when a point is added, as a reminder to add it to `POINTS`, `opens` or `keys`.
const _: fn(FaultPoint) = |p| match p {
    FaultPoint::BeforeAppend
    | FaultPoint::AfterFsync
    | FaultPoint::AfterAck
    | FaultPoint::MidFold
    | FaultPoint::DuringOpen => {}
    FaultPoint::BeforeSchemaCommit
    | FaultPoint::BeforeBootstrapCommit
    | FaultPoint::AfterBootstrapCommit
    | FaultPoint::BeforeGenesis
    | FaultPoint::AfterGenesis
    | FaultPoint::BeforeRename
    | FaultPoint::AfterRename
    | FaultPoint::BeforeMarkCommit
    | FaultPoint::AfterMarkCommit
    | FaultPoint::BeforeUndo
    | FaultPoint::BeforeUndoCommit
    | FaultPoint::AfterUndo
    | FaultPoint::BeforeSeedCommit
    | FaultPoint::AfterSeedCommit => {}
    FaultPoint::BeforeCreateCommit
    | FaultPoint::AfterCreateCommit
    | FaultPoint::BeforeRevokeCommit
    | FaultPoint::AfterRevokeCommit
    | FaultPoint::BeforeExpiryCommit
    | FaultPoint::AfterExpiryCommit => {}
};

/// splitmix64: the seed is all a run depends on, so a run replays.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let z = (self.0 ^ (self.0 >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        let z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// In `0..n`.
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    /// In `-max..=max`.
    fn step(&mut self, max: u64) -> i64 {
        i64::try_from(self.below(2 * max + 1)).expect("small") - i64::try_from(max).expect("small")
    }
}

/// A workload the seed picks: emits and batches over a few keys, with refusals, lists and clock
/// steps both ways between them, and at least one append.
fn random_workload(rng: &mut Rng) -> Vec<Request> {
    const KEYS: [&str; 4] = ["k1", "k2", "k3", "k4"];
    let mut n = 0;
    let mut item = |rng: &mut Rng| {
        n += 1;
        let refused = rng.below(8) == 0;
        Item {
            name: if refused {
                REFUSED_NAME
            } else {
                "crash.random"
            },
            key: (rng.below(2) == 0).then(|| KEYS[rng.below(4) as usize]),
            marker: format!("m{n}"),
        }
    };
    let mut requests: Vec<Request> = (0..1 + rng.below(12))
        .map(|_| match rng.below(20) {
            0..8 => Request::Emit(item(rng)),
            8..14 => Request::Batch((0..1 + rng.below(6)).map(|_| item(rng)).collect()),
            14..17 => Request::List(1 + rng.below(4) as u32),
            _ => Request::Clock(rng.step(10_000)),
        })
        .collect();
    if appends(&requests) == 0 {
        // Unkeyed and allowed, so always new.
        requests.push(Request::Emit(Item {
            name: "crash.random",
            key: None,
            ..item(rng)
        }));
    }
    requests
}

/// Kills the child inside an open, as `opens::kill_seeded` does, around a key change, as
/// `keys::kill_seeded` does, or while its ledger works: which, the seed picks.
fn kill_seeded(seed: u64) {
    let mut rng = Rng(seed);
    match rng.below(3) {
        0 => opens::kill_seeded(&mut rng),
        1 => keys::kill_seeded(&mut rng),
        _ => kill_working(&mut rng),
    }
}

/// Kills the child at the point and on the append the seed picks, checks it as `kill_at` does,
/// then opens once more with the clock behind the last event and checks a new event takes the
/// last event's time.
fn kill_working(rng: &mut Rng) {
    let workload = random_workload(rng);
    let point = POINTS[rng.below(POINTS.len() as u64) as usize];
    let append = 1 + rng.below(appends(&workload));
    let behind = 1 + rng.below(60_000);
    let dir = tempfile::tempdir().expect("a temporary directory");
    let (_, home, shown) = kill_at(dir.path(), point, append, workload);

    let last = shown.events.last().map(|e| e.time);
    let at = last.map_or(clock().now(), |t| {
        Timestamp::from_millis(t.as_millis() - i64::try_from(behind).expect("small"))
    });
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("a runtime");
    let (time, by) = runtime.block_on(async {
        let engine = home.open(ManualClock::new(at)).await;
        let cx = home.call();
        let emit = EmitRequest {
            name: "crash.after".into(),
            data: None,
            metadata: None,
            version: None,
            idempotency_key: None,
        };
        let id = engine.emit(&cx, emit).await.expect("emits").event_id;
        let event = engine
            .get_event(
                &cx,
                GetEventRequest {
                    event_id: id.to_string(),
                },
            )
            .await
            .expect("reads back");
        engine.close().await;
        (event.time, event.emitted_by.to_string())
    });
    assert_eq!(by, emitter(SEED), "emitted after the reopen");
    // Behind the last event it takes that event's time; with none, the clock's.
    assert_eq!(
        time,
        last.unwrap_or(at),
        "reopened {behind} ms behind the last event at {last:?}"
    );
}

/// Prints a failing kill's seed and how to replay it.
struct ReplayHint(u64);

impl Drop for ReplayHint {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!(
                "seeded kill failed: replay it with CRASH_SEED={} CRASH_KILLS=1",
                self.0
            );
        }
    }
}

/// `CRASH_KILLS` seeded kills, 100 by default; kill `i` takes seed `CRASH_SEED + i`, the
/// first seed random unless set.
#[test]
fn seeded_random_kills() {
    let _alone = alone();
    let var = |name| {
        std::env::var(name).ok().map(|v: String| {
            v.parse::<u64>()
                .unwrap_or_else(|_| panic!("{name} is a number: {v:?}"))
        })
    };
    let kills = var("CRASH_KILLS").unwrap_or(100);
    let first = var("CRASH_SEED").unwrap_or_else(|| {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH);
        now.expect("after the epoch").as_nanos() as u64
    });
    // Straight to stderr, past the harness' capture, so a run that hangs still shows its seed.
    writeln!(
        std::io::stderr(),
        "seeded kills: CRASH_SEED={first} CRASH_KILLS={kills}"
    )
    .expect("stderr writes");
    for i in 0..kills {
        let seed = first.wrapping_add(i);
        let _hint = ReplayHint(seed);
        kill_seeded(seed);
    }
}
