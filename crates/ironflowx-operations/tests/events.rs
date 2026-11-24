//! Emitting and reading through the engine: each call is admitted, and every event records the
//! key that emitted it (TEN-1, TEN-6).

use std::path::Path;

use ironflowx_cell::ManualClock;
use ironflowx_core::{ErrorKind, Timestamp};
use ironflowx_operations::Engine;
use ironflowx_proto::{
    Call, EmitBatchRequest, EmitRequest, Event, GetEventRequest, ListEventsRequest, Operations,
};

fn emit(name: &str) -> EmitRequest {
    EmitRequest {
        name: name.into(),
        data: None,
        metadata: None,
        version: None,
        idempotency_key: None,
    }
}

const fn first_page() -> ListEventsRequest {
    ListEventsRequest {
        limit: None,
        cursor: None,
    }
}

/// Opens a fresh engine at `root`: the engine, its bootstrap key's call, and that key's
/// attribution, taken from the minted text (`ixk_<key-id>_<secret>`), not from the engine.
async fn open(root: &Path) -> (Engine, Call, String) {
    let clock = ManualClock::new(Timestamp::from_millis(1_800_000_000_000));
    let opened = Engine::open(root, clock, None).await.unwrap();
    let minted = opened.minted.unwrap();
    let by = format!("key:{}", &minted.expose()[4..40]);
    (opened.engine, Call::with_key(minted), by)
}

async fn events(engine: &Engine, cx: &Call) -> Vec<Event> {
    engine.list_events(cx, first_page()).await.unwrap().events
}

#[tokio::test]
async fn the_bootstrap_key_emits_and_reads_its_own_events() {
    let dir = tempfile::tempdir().unwrap();
    let (engine, cx, by) = open(dir.path()).await;

    let one = engine.emit(&cx, emit("order.placed")).await.unwrap();
    let batch = engine
        .emit_batch(
            &cx,
            EmitBatchRequest {
                items: vec![emit("a.one"), emit("a.two"), emit("a.three")],
            },
        )
        .await
        .unwrap();
    assert!(batch.items.iter().all(Result::is_ok), "{batch:?}");

    let get = || GetEventRequest {
        event_id: one.event_id.to_string(),
    };
    let mut ids = vec![one.event_id.to_string()];
    ids.extend(batch.items.iter().flatten().map(|e| e.event_id.to_string()));
    let names = ["order.placed", "a.one", "a.two", "a.three"];
    assert_eq!(ids.len(), names.len());
    for (event_id, name) in ids.into_iter().zip(names) {
        let read = engine
            .get_event(
                &cx,
                GetEventRequest {
                    event_id: event_id.clone(),
                },
            )
            .await
            .unwrap();
        assert_eq!(
            (
                read.event_id.to_string(),
                read.name.as_str(),
                read.emitted_by.to_string()
            ),
            (event_id, name, by.clone())
        );
    }

    let listed = events(&engine, &cx).await;
    let names: Vec<_> = listed.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["a.three", "a.two", "a.one", "order.placed"]);
    assert!(listed.iter().all(|e| e.emitted_by.to_string() == by));

    let anon = Call::anonymous();
    let refusals = [
        engine.emit(&anon, emit("x.one")).await.err(),
        engine
            .emit_batch(
                &anon,
                EmitBatchRequest {
                    items: vec![emit("x.two")],
                },
            )
            .await
            .err(),
        engine.get_event(&anon, get()).await.err(),
        engine.list_events(&anon, first_page()).await.err(),
    ];
    for err in refusals {
        assert_eq!(err.map(|e| e.kind), Some(ErrorKind::Unauthenticated));
    }
    // A refused emit wrote nothing.
    assert_eq!(events(&engine, &cx).await, listed);
    engine.close().await;
}

#[tokio::test]
async fn two_engines_in_one_process_each_attribute_to_their_own_key() {
    let (dir_a, dir_b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (a, cx_a, by_a) = open(dir_a.path()).await;
    let (b, cx_b, by_b) = open(dir_b.path()).await;
    assert_ne!(by_a, by_b);

    // Interleaved, so neither engine's attribution can come from whichever ran last.
    a.emit(&cx_a, emit("a.first")).await.unwrap();
    b.emit(&cx_b, emit("b.first")).await.unwrap();
    a.emit(&cx_a, emit("a.second")).await.unwrap();

    for (engine, cx, by, count) in [(&a, &cx_a, &by_a, 2), (&b, &cx_b, &by_b, 1)] {
        let listed = events(engine, cx).await;
        assert_eq!(listed.len(), count);
        assert!(
            listed.iter().all(|e| e.emitted_by.to_string() == *by),
            "{listed:?}"
        );
    }
    a.close().await;
    b.close().await;
}
