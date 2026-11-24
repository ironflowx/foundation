//! The child side of the crash test (#6). Usage: `crash-child <root> <point> <crossing>
//! [staged|renamed]`, its stdin a socket.
//!
//! The first stdin line is the seed to open with, or `-` for none. Secrets pass only over that
//! socket, never in an argument or over stdout: as an open or `create_key` comes to store a key,
//! before any point that storing reaches, the child writes `<env-ref> <key-id> <secret>|-` back
//! over it, the secret unless the key is the seed.
//!
//! The child opens an engine on `<root>` with the seed and a manual clock, creates a second key
//! with it, and prints `ready`, then `<env-ref> <second-key-id>`. With `staged`, the first ledger
//! to append its genesis record finds its read store's place taken, so that a bootstrapping open
//! fails in staging. With `renamed`, a ledger that has reached its final name finds its lock held,
//! by the child, so that the open fails then.
//!
//! Then, per stdin line, `<caller>` being `1` for the seed, `2` for the second key, or the ID of a
//! key a `create` made:
//!
//! - `emit <caller> <item>` emits one item and prints `<result>`: `new:<event-id>`,
//!   `dup:<event-id>` or `err:<kind>`;
//! - `batch <caller> <item> <item> ...` emits a batch and prints `<result> <result> ...`, one per
//!   item in input order, or `err:<kind>` for the whole batch;
//! - `list <limit> <cursor>|-` lists a page and prints `page <next>|- <event-id> ...`, or
//!   `err:<kind>`;
//! - `clock <millis>` steps the manual clock by `<millis>`, which may be negative, and prints
//!   `ok`;
//! - `create <caller> <expiry>|- <millis>` creates a key with every scope and that expiry,
//!   stepping the clock by `<millis>` as the key is told: after the caller is admitted and before
//!   the key's transaction. It prints `key <key-id>` or `err:<kind>`, and dies if the
//!   secret returned is not the one told;
//! - `revoke <caller> <key-id>` revokes a key and prints `ok` or `err:<kind>`;
//! - `whoami <caller>` prints `ok` or `err:<kind>`;
//! - `arm <point>` prints `ok`; the next pass of `<point>` then stops, as below.
//!
//! An item is `<name>,<key>|-,<marker>`: the marker goes in the metadata as
//! `{"marker":"<marker>"}`, so the parent finds the event without its ID.
//!
//! While a request runs, a `probe` line is answered `pending`; see `probing`.
//!
//! On its `<crossing>`-th pass of the fault point named `<point>`, `-` for none, it prints
//! `at <point>` and waits there for the parent to kill it: the barrier that makes the kill land
//! exactly at the point (D23). A point inside an open is before `ready`, which it then never
//! prints.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::future::{Future, poll_fn};
use std::io::{BufRead, Write};
use std::num::NonZeroU32;
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::pin::pin;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::task::Poll;

use ironflowx_cell::ManualClock;
use ironflowx_core::{
    EnvRef, FaultPoint, KeyId, Scope, Secret, Timestamp, set_fault_hook, set_key_hook,
};
use ironflowx_operations::Engine;
use ironflowx_proto::{
    Call, CreateKeyRequest, EmitBatchRequest, EmitRequest, EmitResponse, ListEnvironmentsRequest,
    ListEventsRequest, OpError, Operations, RevokeKeyRequest, WhoamiRequest,
};
use tokio::sync::mpsc::{self, UnboundedReceiver};

/// The point to stop at, and on which pass of it.
static STOP: OnceLock<(String, u64)> = OnceLock::new();
static CROSSINGS: AtomicU64 = AtomicU64::new(0);
/// The point an `arm` request named.
static ARMED: OnceLock<String> = OnceLock::new();
/// The engine's clock, and how far it steps as the next key is told.
static CLOCK: OnceLock<ManualClock> = OnceLock::new();
static STEP: AtomicI64 = AtomicI64::new(0);
/// The secret last told.
static TOLD: Mutex<String> = Mutex::new(String::new());
/// What makes the open fail: `staged` or `renamed`, and `ledgers/`.
static OBSTRUCT: OnceLock<(String, PathBuf)> = OnceLock::new();
/// Held until the kill.
static LOCK: OnceLock<File> = OnceLock::new();

/// Puts what the engine's open trips over in its way.
fn obstruct(point: FaultPoint) {
    let Some((how, ledgers)) = OBSTRUCT.get() else {
        return;
    };
    match (how.as_str(), point) {
        ("staged", FaultPoint::AfterGenesis) => {
            let path = ledgers.join(".staging/read.db");
            std::fs::create_dir(path).expect("the read store's place is free");
        }
        ("renamed", FaultPoint::AfterRename) => {
            let mut entries = std::fs::read_dir(ledgers).expect("ledgers/ reads");
            let ledger = entries.next().expect("a ledger").expect("an entry").path();
            let lock = File::open(ledger.join("ledger.lock")).expect("the ledger's lock file");
            lock.try_lock().expect("the lock is free");
            LOCK.set(lock).expect("set once");
        }
        _ => {}
    }
}

/// Tells the parent, over the socket, which key is about to be stored, then steps the clock as a
/// `create` asked.
fn tell(env: EnvRef, key: KeyId, secret: Option<&Secret>) {
    let socket = std::io::stdin().as_fd().try_clone_to_owned();
    let line = format!("{env} {key} {}\n", secret.map_or("-", Secret::expose));
    UnixStream::from(socket.expect("the socket duplicates"))
        .write_all(line.as_bytes())
        .expect("the socket writes");
    *TOLD.lock().expect("no panic") = secret.map_or("", Secret::expose).to_owned();
    let clock = CLOCK.get().expect("set before the engine opens");
    clock.step(STEP.swap(0, Ordering::SeqCst));
}

fn at_point(point: FaultPoint) {
    obstruct(point);
    let (name, crossing) = STOP.get().expect("set before the engine opens");
    let armed = ARMED.get().is_some_and(|a| a == point.name());
    let crossed = point.name() == name && CROSSINGS.fetch_add(1, Ordering::SeqCst) + 1 == *crossing;
    if !armed && !crossed {
        return;
    }
    let mut out = std::io::stdout().lock();
    writeln!(out, "at {}", point.name()).expect("the pipe writes");
    out.flush().expect("the pipe flushes");
    drop(out);
    // Hold this thread at the point until the parent's SIGKILL.
    loop {
        std::thread::park();
    }
}

fn item(text: &str) -> EmitRequest {
    let [name, key, marker] = text.split(',').collect::<Vec<_>>()[..] else {
        panic!("an item is <name>,<key>|-,<marker>: {text:?}");
    };
    // It goes into the JSON unescaped.
    assert!(
        marker.bytes().all(|b| b.is_ascii_alphanumeric()),
        "a marker is ASCII letters and digits: {marker:?}"
    );
    EmitRequest {
        name: name.to_owned(),
        data: None,
        metadata: Some(format!(r#"{{"marker":"{marker}"}}"#).into_bytes()),
        version: None,
        idempotency_key: (key != "-").then(|| key.to_owned()),
    }
}

fn encode(r: &Result<EmitResponse, OpError>) -> String {
    match r {
        Ok(r) if r.duplicate => format!("dup:{}", r.event_id),
        Ok(r) => format!("new:{}", r.event_id),
        Err(e) => format!("err:{:?}", e.kind),
    }
}

fn done<T>(r: Result<T, OpError>) -> String {
    r.map_or_else(|e| format!("err:{:?}", e.kind), |_| "ok".to_owned())
}

/// Runs `request`, answering each `probe` line that arrives meanwhile with `pending`. It polls
/// `request` before each probe, so a reply sent before the probe is seen: stopped at a point,
/// the writer sends nothing more, which makes "not answered yet" exact rather than timed.
async fn probing(
    request: impl Future<Output = String>,
    lines: &mut UnboundedReceiver<String>,
) -> String {
    let mut request = pin!(request);
    poll_fn(|cx| {
        loop {
            if let Poll::Ready(answer) = request.as_mut().poll(cx) {
                return Poll::Ready(answer);
            }
            match lines.poll_recv(cx) {
                Poll::Ready(Some(line)) if line == "probe" => {
                    println!("pending");
                }
                Poll::Ready(Some(line)) => panic!("{line:?} arrived mid-request"),
                // Stdin closed: the parent is gone and kills us next.
                Poll::Ready(None) | Poll::Pending => return Poll::Pending,
            }
        }
    })
    .await
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (root, point, crossing) = match &args[..] {
        [root, point, crossing] => (PathBuf::from(root), point, crossing),
        [root, point, crossing, how] if how == "staged" || how == "renamed" => {
            let root = PathBuf::from(root);
            let set = OBSTRUCT.set((how.clone(), root.join("ledgers")));
            set.expect("set once");
            (root, point, crossing)
        }
        _ => panic!("usage: crash-child <root> <point> <crossing> [staged|renamed]"),
    };
    let crossing = crossing.parse().expect("a crossing number");
    STOP.set((point.clone(), crossing)).expect("set once");
    set_fault_hook(at_point);
    set_key_hook(tell);
    let mut seed = String::new();
    std::io::stdin()
        .read_line(&mut seed)
        .expect("the socket reads");
    let seed = seed.trim_end();
    let secret = || Secret::new(seed.to_owned());

    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("a runtime");
    runtime.block_on(async {
        let clock = ManualClock::new(Timestamp::from_millis(1_700_000_000_000));
        assert!(CLOCK.set(clock.clone()).is_ok(), "set once");
        let opened = Engine::open(&root, clock.clone(), (seed != "-").then(secret))
            .await
            .expect("the engine opens");
        let engine = opened.engine;
        let first = Call::with_key(secret());
        let envs = engine
            .list_environments(&first, ListEnvironmentsRequest {})
            .await
            .expect("the seed lists its environment");
        let write = CreateKeyRequest {
            scopes: BTreeSet::from([Scope::Write]),
            ..CreateKeyRequest::default()
        };
        let created = engine.create_key(&first, write).await;
        let created = created.expect("the seed creates a key");
        let mut out = std::io::stdout();
        writeln!(out, "ready").expect("the pipe writes");
        writeln!(out, "{} {}", envs[0].env, created.info.key_id).expect("the pipe writes");
        let mut callers = BTreeMap::from([
            ("1".to_owned(), first),
            ("2".to_owned(), Call::with_key(created.key)),
        ]);
        // Stdin is read on its own thread, so a `probe` reaches the runtime mid-request.
        let (send, mut lines) = mpsc::unbounded_channel();
        std::thread::spawn(move || {
            for line in std::io::stdin().lock().lines() {
                if send.send(line.expect("the pipe reads")).is_err() {
                    break;
                }
            }
        });
        while let Some(line) = lines.recv().await {
            let (verb, rest) = line.split_once(' ').unwrap_or((&line, ""));
            let request = async {
                match verb {
                    "emit" => {
                        let (by, text) = rest.split_once(' ').expect("emit <caller> <item>");
                        encode(&engine.emit(&callers[by], item(text)).await)
                    }
                    "batch" => {
                        let (by, items) = rest.split_once(' ').expect("batch <caller> <item> ...");
                        let items = items.split(' ').map(item).collect();
                        let batch = EmitBatchRequest { items };
                        match engine.emit_batch(&callers[by], batch).await {
                            Ok(r) => r.items.iter().map(encode).collect::<Vec<_>>().join(" "),
                            Err(e) => encode(&Err(e)),
                        }
                    }
                    "list" => {
                        let (limit, cursor) = rest.split_once(' ').expect("list <limit> <cursor>");
                        let req = ListEventsRequest {
                            limit: NonZeroU32::new(limit.parse().expect("a limit")),
                            cursor: (cursor != "-").then(|| cursor.to_owned()),
                        };
                        match engine.list_events(&callers["1"], req).await {
                            Ok(page) => {
                                let mut words = vec![page.next.unwrap_or_else(|| "-".into())];
                                words.extend(page.events.iter().map(|e| e.event_id.to_string()));
                                format!("page {}", words.join(" "))
                            }
                            Err(e) => format!("err:{:?}", e.kind),
                        }
                    }
                    "clock" => {
                        clock.step(rest.parse().expect("a step in milliseconds"));
                        "ok".to_owned()
                    }
                    "create" => {
                        let [by, expiry, step] = rest.split(' ').collect::<Vec<_>>()[..] else {
                            panic!("create <caller> <expiry>|- <millis>");
                        };
                        let expiry = (expiry != "-").then(|| expiry.parse().expect("an expiry"));
                        let req = CreateKeyRequest {
                            scopes: BTreeSet::from([Scope::Read, Scope::Write, Scope::Admin]),
                            expires_at: expiry.map(Timestamp::from_millis),
                            label: None,
                        };
                        let step = step.parse().expect("a step in milliseconds");
                        STEP.store(step, Ordering::SeqCst);
                        match engine.create_key(&callers[by], req).await {
                            Ok(created) => {
                                let same = *TOLD.lock().expect("no panic") == created.key.expose();
                                assert!(same, "a created key's secret is the one told");
                                let id = created.info.key_id.to_string();
                                callers.insert(id.clone(), Call::with_key(created.key));
                                format!("key {id}")
                            }
                            Err(e) => format!("err:{:?}", e.kind),
                        }
                    }
                    "revoke" => {
                        let (by, key) = rest.split_once(' ').expect("revoke <caller> <key-id>");
                        let req = RevokeKeyRequest {
                            key_id: key.to_owned(),
                        };
                        done(engine.revoke_key(&callers[by], req).await)
                    }
                    "whoami" => done(engine.whoami(&callers[rest], WhoamiRequest {}).await),
                    "arm" => {
                        ARMED.set(rest.to_owned()).expect("armed once");
                        "ok".to_owned()
                    }
                    _ => panic!("unknown request {line:?}"),
                }
            };
            let answer = probing(request, &mut lines).await;
            writeln!(out, "{answer}").expect("the pipe writes");
        }
        engine.close().await;
    });
}
