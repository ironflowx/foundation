//! Emitting and reading through the engine: each call is admitted, and every event records the
//! key that emitted it (TEN-1, TEN-6).

use ironflowx_cell::ManualClock;
use ironflowx_core::{ErrorKind, Timestamp};
use ironflowx_operations::Engine;
use ironflowx_proto::{Call, EmitBatchRequest, EmitRequest, GetEventRequest, ListEventsRequest};

fn emit(name: &str) -> EmitRequest {
    EmitRequest {
        name: name.into(),
        data: None,
        metadata: None,
        version: None,
        idempotency_key: None,
    }
}

#[tokio::test]
async fn the_bootstrap_key_emits_and_reads_its_own_events() {
    let dir = tempfile::tempdir().unwrap();
    let clock = ManualClock::new(Timestamp::from_millis(1_800_000_000_000));
    let opened = Engine::open(dir.path(), clock).await.unwrap();
    let engine = &opened.engine;
    let minted = opened.minted.unwrap();
    // `ixk_<key-id>_<secret>`: the ID as minted, not as the engine reports it.
    let by = format!("key:{}", &minted.expose()[4..40]);
    let cx = Call::with_key(minted);

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

    let read = engine
        .get_event(
            &cx,
            GetEventRequest {
                event_id: one.event_id.to_string(),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        (read.name.as_str(), read.emitted_by.to_string()),
        ("order.placed", by.clone())
    );

    let page = engine
        .list_events(
            &cx,
            ListEventsRequest {
                limit: None,
                cursor: None,
            },
        )
        .await
        .unwrap();
    let names: Vec<_> = page.events.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["a.three", "a.two", "a.one", "order.placed"]);
    assert!(page.events.iter().all(|e| e.emitted_by.to_string() == by));

    let anon = Call::anonymous();
    let refusals = [
        engine.emit(&anon, emit("x.y")).await.err(),
        engine
            .emit_batch(&anon, EmitBatchRequest { items: vec![] })
            .await
            .err(),
        engine
            .get_event(
                &anon,
                GetEventRequest {
                    event_id: one.event_id.to_string(),
                },
            )
            .await
            .err(),
        engine
            .list_events(
                &anon,
                ListEventsRequest {
                    limit: None,
                    cursor: None,
                },
            )
            .await
            .err(),
    ];
    for err in refusals {
        assert_eq!(err.map(|e| e.kind), Some(ErrorKind::Unauthenticated));
    }
    opened.engine.close().await;
}
