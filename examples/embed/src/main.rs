//! Opens a ledger, emits one event and reads it back, using only `ironflowx` and `tokio`.

use ironflowx::cell::{HostClock, Ledger, LocalOperations, new_env_ref};
use ironflowx::core::Clock;
use ironflowx::proto::{Call, EmitRequest, Event, GetEventRequest, Operations};

fn main() {
    let env = new_env_ref();
    let dir = std::env::temp_dir().join(format!("ironflowx-embed-{env}"));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("a runtime");
    runtime.block_on(async {
        let ledger = Ledger::open(&dir, env, HostClock)
            .await
            .expect("the ledger opens");
        let ops = LocalOperations::new(&ledger);
        let cx = Call::anonymous();
        let before = HostClock.now();
        let emitted = ops
            .emit(
                &cx,
                EmitRequest {
                    name: "order.placed".to_owned(),
                    data: Some(br#"{"total":42}"#.to_vec()),
                    metadata: Some(br#"{"source":"embed"}"#.to_vec()),
                    version: None,
                    idempotency_key: Some("order-1".to_owned()),
                },
            )
            .await
            .expect("the emit is acknowledged");
        let after = HostClock.now();
        assert!(!emitted.duplicate, "a new key stores a new event");
        let event = ops
            .get_event(
                &cx,
                GetEventRequest {
                    event_id: emitted.event_id.to_string(),
                },
            )
            .await
            .expect("the event reads back");
        assert!(
            (before..=after).contains(&event.time),
            "the event takes the host clock's time"
        );
        assert_eq!(
            event,
            Event {
                event_id: emitted.event_id,
                name: "order.placed".to_owned(),
                data: br#"{"total":42}"#.to_vec(),
                metadata: Some(br#"{"source":"embed"}"#.to_vec()),
                version: 1,
                idempotency_key: Some("order-1".to_owned()),
                time: event.time,
                emitted_by: "embedder:local-operations".parse().expect("an emitter"),
            }
        );
        ledger.close().await;
    });
    std::fs::remove_dir_all(&dir).expect("the ledger directory is removed");
    println!("embed: opened a ledger, emitted and read back one event");
}
