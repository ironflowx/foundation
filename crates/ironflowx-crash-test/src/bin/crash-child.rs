//! The child side of the crash test (#6). Usage: `crash-child <root> <point> <crossing>
//! [obstruct]`, its stdin a socket.
//!
//! The first stdin line is the seed to open with, or `-` for none. Secrets pass only over that
//! socket, never in an argument or over stdout: as an open comes to store a key, before any point
//! that storing reaches, the child writes `<env-ref> <key-id> <secret>|-` back over it, the secret
//! only if the engine minted it.
//!
//! The child opens an engine on `<root>` with the seed and a manual clock, creates a second key
//! with it, and prints `ready`, then `<env-ref> <second-key-id>`. With `obstruct`, the first ledger
//! to append its genesis record finds its read store's place taken, so that a bootstrapping open
//! fails in staging and undoes itself.
//!
//! Then, per stdin line, `<caller>` being `1` for the seed or `2` for the second key:
//!
//! - `emit <caller> <item>` emits one item and prints `<result>`: `new:<event-id>`,
//!   `dup:<event-id>` or `err:<kind>`;
//! - `batch <caller> <item> <item> ...` emits a batch and prints `<result> <result> ...`, one per
//!   item in input order, or `err:<kind>` for the whole batch;
//! - `list <limit> <cursor>|-` lists a page and prints `page <next>|- <event-id> ...`, or
//!   `err:<kind>`;
//! - `clock <millis>` steps the manual clock by `<millis>`, which may be negative, and prints
//!   `ok`.
//!
//! An item is `<name>,<key>|-,<marker>`: the marker goes in the metadata as
//! `{"marker":"<marker>"}`, so the parent finds the event without its ID.
//!
//! While a request runs, a `probe` line is answered `pending`; see `probing`.
//!
//! On its `<crossing>`-th pass of the fault point named `<point>`, it prints `at <point>` and
//! waits there for the parent to kill it: the barrier that makes the kill land exactly at the
//! point (D23). A point inside an open is before `ready`, which it then never prints.

use std::collections::BTreeSet;
use std::future::{Future, poll_fn};
use std::io::{BufRead, Write};
use std::num::NonZeroU32;
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::pin::pin;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::Poll;

use ironflowx_cell::ManualClock;
use ironflowx_core::{
    EnvRef, FaultPoint, KeyId, Scope, Secret, Timestamp, set_fault_hook, set_identity_hook,
};
use ironflowx_operations::Engine;
use ironflowx_proto::{
    Call, CreateKeyRequest, EmitBatchRequest, EmitRequest, EmitResponse, ListEnvironmentsRequest,
    ListEventsRequest, OpError, Operations,
};
use tokio::sync::mpsc::{self, UnboundedReceiver};

/// The point to stop at, and on which pass of it.
static STOP: OnceLock<(String, u64)> = OnceLock::new();
static CROSSINGS: AtomicU64 = AtomicU64::new(0);
/// Where to put a directory in a new ledger's way.
static OBSTRUCT: OnceLock<PathBuf> = OnceLock::new();

/// Tells the parent, over the socket, which key is about to be stored.
fn tell(env: EnvRef, key: KeyId, secret: Option<&Secret>) {
    let socket = std::io::stdin().as_fd().try_clone_to_owned();
    let line = format!("{env} {key} {}\n", secret.map_or("-", Secret::expose));
    UnixStream::from(socket.expect("the socket duplicates"))
        .write_all(line.as_bytes())
        .expect("the socket writes");
}

fn at_point(point: FaultPoint) {
    if point == FaultPoint::AfterGenesis
        && let Some(path) = OBSTRUCT.get()
    {
        std::fs::create_dir(path).expect("the read store's place is free");
    }
    let (name, crossing) = STOP.get().expect("set before the engine opens");
    if point.name() != name || CROSSINGS.fetch_add(1, Ordering::SeqCst) + 1 != *crossing {
        return;
    }
    let mut out = std::io::stdout().lock();
    writeln!(out, "at {name}").expect("the pipe writes");
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
        [root, point, crossing, obstruct] if obstruct == "obstruct" => {
            let root = PathBuf::from(root);
            let path = root.join("ledgers/.staging/read.db");
            OBSTRUCT.set(path).expect("set once");
            (root, point, crossing)
        }
        _ => panic!("usage: crash-child <root> <point> <crossing> [obstruct]"),
    };
    let crossing = crossing.parse().expect("a crossing number");
    STOP.set((point.clone(), crossing)).expect("set once");
    set_fault_hook(at_point);
    set_identity_hook(tell);
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
        let second = Call::with_key(created.key);
        let caller = |n: &str| match n {
            "1" => &first,
            "2" => &second,
            _ => panic!("no caller {n:?}"),
        };
        let mut out = std::io::stdout();
        writeln!(out, "ready").expect("the pipe writes");
        writeln!(out, "{} {}", envs[0].env, created.info.key_id).expect("the pipe writes");
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
                        encode(&engine.emit(caller(by), item(text)).await)
                    }
                    "batch" => {
                        let (by, items) = rest.split_once(' ').expect("batch <caller> <item> ...");
                        let items = items.split(' ').map(item).collect();
                        let batch = EmitBatchRequest { items };
                        match engine.emit_batch(caller(by), batch).await {
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
                        match engine.list_events(&first, req).await {
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
                    _ => panic!("unknown request {line:?}"),
                }
            };
            let answer = probing(request, &mut lines).await;
            writeln!(out, "{answer}").expect("the pipe writes");
        }
        engine.close().await;
    });
}
