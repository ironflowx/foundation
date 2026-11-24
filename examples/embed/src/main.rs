//! Bootstraps an engine, emits one event with the bootstrap key, creates a read-only key, reads
//! the event back with it and sees its emit refused, using only `ironflowx` and `tokio`.

use ironflowx::cell::{HostClock, new_env_ref};
use ironflowx::core::{Clock, ErrorKind};
use ironflowx::operations::Engine;
use ironflowx::proto::{
    Call, CreateKeyRequest, EmitRequest, Event, GetEventRequest, OpError, Operations,
};

fn main() {
    let root = std::env::temp_dir().join(format!("ironflowx-embed-{}", new_env_ref()));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("a runtime");
    runtime.block_on(async {
        let opened = Engine::open(&root, HostClock, None)
            .await
            .expect("the engine opens");
        let engine = opened.engine;
        // The bootstrap key, returned only by this first open.
        let admin = Call::with_key(opened.minted.expect("the first open mints a key"));
        let order = EmitRequest {
            name: "order.placed".to_owned(),
            data: Some(br#"{"total":42}"#.to_vec()),
            metadata: Some(br#"{"source":"embed"}"#.to_vec()),
            version: None,
            idempotency_key: Some("order-1".to_owned()),
        };
        let before = HostClock.now();
        let emitted = engine
            .emit(&admin, order.clone())
            .await
            .expect("the emit is acknowledged");
        let after = HostClock.now();
        assert!(!emitted.duplicate, "a new key stores a new event");
        // No scopes asked for: the key holds `read`.
        let created = engine
            .create_key(&admin, CreateKeyRequest::default())
            .await
            .expect("the bootstrap key creates a key");
        let reader = Call::with_key(created.key);
        let event = engine
            .get_event(
                &reader,
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
        let bootstrap = created.info.created_by.expect("the creating key");
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
                emitted_by: format!("key:{bootstrap}").parse().expect("an emitter"),
            }
        );
        let refused = engine
            .emit(&reader, order)
            .await
            .expect_err("a `read` key cannot emit");
        assert_eq!(
            refused,
            OpError {
                kind: ErrorKind::PermissionDenied,
                message: "missing scope: write".to_owned(),
                hint: Some("create a key with scope write, or use one that has it".to_owned()),
            }
        );
        engine.close().await;
    });
    std::fs::remove_dir_all(&root).expect("the data directory is removed");
    println!(
        "embed: bootstrapped an engine, emitted one event, read it back with a read key and saw \
         that key's emit refused"
    );
}
