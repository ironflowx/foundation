//! The child side of the crash test (#6). Usage: `crash-child <dir> <env-ref> <point> <crossing>`.
//!
//! Opens the ledger in `<dir>` with a manual clock and prints `ready`. Then, per stdin line:
//!
//! - `emit <item>` emits one item and prints `<result>`: `new:<event-id>`, `dup:<event-id>`
//!   or `err:<kind>`;
//! - `batch <item> <item> ...` emits a batch and prints `<result> <result> ...`, one per item
//!   in input order, or `err:<kind>` for the whole batch;
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
//! point (D23).

use std::future::{Future, poll_fn};
use std::io::{BufRead, Write};
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::pin::pin;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::Poll;

use ironflowx_cell::{FaultPoint, Ledger, LocalOperations, ManualClock, set_fault_hook};
use ironflowx_core::{EnvRef, Timestamp};
use ironflowx_proto::{
    Call, EmitBatchRequest, EmitRequest, EmitResponse, ListEventsRequest, OpError, Operations,
};
use tokio::sync::mpsc::{self, UnboundedReceiver};

/// The point to stop at, and on which pass of it.
static STOP: OnceLock<(String, u64)> = OnceLock::new();
static CROSSINGS: AtomicU64 = AtomicU64::new(0);

fn at_point(point: FaultPoint) {
    let (name, crossing) = STOP.get().expect("set before the ledger opens");
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
    let [dir, env, point, crossing] = &args[..] else {
        panic!("usage: crash-child <dir> <env-ref> <point> <crossing>");
    };
    let dir = PathBuf::from(dir);
    let env: EnvRef = env.parse().expect("an environment reference");
    let crossing = crossing.parse().expect("a crossing number");
    STOP.set((point.clone(), crossing)).expect("set once");
    set_fault_hook(at_point);

    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("a runtime");
    runtime.block_on(async {
        let clock = ManualClock::new(Timestamp::from_millis(1_700_000_000_000));
        let ledger = Ledger::open(&dir, env, clock.clone())
            .await
            .expect("the ledger opens");
        let ops = LocalOperations::new(&ledger);
        let cx = Call::anonymous();
        let mut out = std::io::stdout();
        writeln!(out, "ready").expect("the pipe writes");
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
                    "emit" => encode(&ops.emit(&cx, item(rest)).await),
                    "batch" => {
                        let items = rest.split(' ').map(item).collect();
                        match ops.emit_batch(&cx, EmitBatchRequest { items }).await {
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
                        match ops.list_events(&cx, req).await {
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
        ledger.close().await;
    });
}
