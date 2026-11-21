//! The child side of the crash test (#6). Usage: `crash-child <dir> <env-ref> <point> <crossing>`.
//!
//! Opens the ledger in `<dir>` with a manual clock and prints `ready`. Then, per stdin line
//! `emit <name>`, emits and prints `ack <event-id>` or `err <kind>`. On its `<crossing>`-th
//! pass of the fault point named `<point>`, it prints `at <point>` and waits there for the
//! parent to kill it: the barrier that makes the kill land exactly at the point (D23).

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use ironflowx_cell::{FaultPoint, Ledger, LocalOperations, ManualClock, set_fault_hook};
use ironflowx_core::{EnvRef, Timestamp};
use ironflowx_proto::{Call, EmitRequest, Operations};

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
            let Some(name) = line.strip_prefix("emit ") else {
                panic!("unknown request {line:?}");
            };
            let req = EmitRequest {
                name: name.to_owned(),
                data: None,
                metadata: None,
                version: None,
                idempotency_key: None,
            };
            match ops.emit(&cx, req).await {
                Ok(r) => writeln!(out, "ack {}", r.event_id),
                Err(e) => writeln!(out, "err {:?}", e.kind),
            }
            .expect("the pipe writes");
        }
        ledger.close().await;
    });
}
