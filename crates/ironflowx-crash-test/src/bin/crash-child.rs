//! The child side of the crash test (#6). Usage: `crash-child <dir> <env-ref> <point> <crossing>`.
//!
//! Opens the ledger in `<dir>` with a manual clock and prints `ready`. Then, per stdin line:
//!
//! - `emit <item>` emits one item and prints `ack <event-id> new|dup` or `err <kind>`;
//! - `batch <item> <item> ...` emits a batch and prints `batch <result> <result> ...`, each
//!   result `new:<event-id>`, `dup:<event-id>` or `err:<kind>`, in input order;
//! - `list <limit> <cursor>|-` lists a page and prints `page <next>|- <event-id> ...`.
//!
//! An item is `<name>,<key>|-,<marker>`: the marker goes in the metadata as
//! `{"marker":"<marker>"}`, so the parent finds the event without its ID.
//!
//! On its `<crossing>`-th pass of the fault point named `<point>`, it prints `at <point>` and
//! waits there for the parent to kill it: the barrier that makes the kill land exactly at the
//! point (D23).

use std::io::{BufRead, Write};
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use ironflowx_cell::{FaultPoint, Ledger, LocalOperations, ManualClock, set_fault_hook};
use ironflowx_core::{EnvRef, Timestamp};
use ironflowx_proto::{
    Call, EmitBatchRequest, EmitRequest, EmitResponse, ListEventsRequest, OpError, Operations,
};

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
        panic!("an item is <name>,<key>,<marker>: {text:?}");
    };
    EmitRequest {
        name: name.to_owned(),
        data: None,
        metadata: Some(format!(r#"{{"marker":"{marker}"}}"#).into_bytes()),
        version: None,
        idempotency_key: (key != "-").then(|| key.to_owned()),
    }
}

fn result(r: &Result<EmitResponse, OpError>) -> String {
    match r {
        Ok(r) if r.duplicate => format!("dup:{}", r.event_id),
        Ok(r) => format!("new:{}", r.event_id),
        Err(e) => format!("err:{:?}", e.kind),
    }
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
        let ledger = Ledger::open(&dir, env, clock)
            .await
            .expect("the ledger opens");
        let ops = LocalOperations::new(&ledger);
        let cx = Call::anonymous();
        let mut out = std::io::stdout();
        writeln!(out, "ready").expect("the pipe writes");
        for line in std::io::stdin().lock().lines() {
            let line = line.expect("the pipe reads");
            let (verb, rest) = line.split_once(' ').unwrap_or((&line, ""));
            let answer = match verb {
                "emit" => match ops.emit(&cx, item(rest)).await {
                    Ok(r) if r.duplicate => format!("ack {} dup", r.event_id),
                    Ok(r) => format!("ack {} new", r.event_id),
                    Err(e) => format!("err {:?}", e.kind),
                },
                "batch" => {
                    let items = rest.split(' ').map(item).collect();
                    match ops.emit_batch(&cx, EmitBatchRequest { items }).await {
                        Ok(r) => {
                            let results: Vec<_> = r.items.iter().map(result).collect();
                            format!("batch {}", results.join(" "))
                        }
                        Err(e) => format!("err {:?}", e.kind),
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
                        Err(e) => format!("err {:?}", e.kind),
                    }
                }
                _ => panic!("unknown request {line:?}"),
            };
            writeln!(out, "{answer}").expect("the pipe writes");
        }
        ledger.close().await;
    });
}
