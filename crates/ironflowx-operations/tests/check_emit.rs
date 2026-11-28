//! `check_emit` answers as an emit does, without a submit (D120): for each event rule's refusal,
//! and for every limit at its edge, it agrees with the engine on the kind and the message.

use ironflowx_cell::ManualClock;
use ironflowx_core::limits::{
    MAX_DATA_BYTES, MAX_EVENT_NAME_BYTES, MAX_IDEMPOTENCY_KEY_BYTES, MAX_METADATA_BYTES,
};
use ironflowx_core::{ErrorKind, Timestamp};
use ironflowx_operations::{Engine, check_emit};
use ironflowx_proto::{Call, EmitRequest, Operations};

fn named(name: &str) -> EmitRequest {
    EmitRequest {
        name: name.into(),
        data: None,
        metadata: None,
        version: None,
        idempotency_key: None,
    }
}

fn with_data(data: &[u8]) -> EmitRequest {
    EmitRequest {
        data: Some(data.to_vec()),
        ..named("a.b")
    }
}

fn with_metadata(metadata: &str) -> EmitRequest {
    EmitRequest {
        metadata: Some(metadata.into()),
        ..named("a.b")
    }
}

fn with_version(version: i64) -> EmitRequest {
    EmitRequest {
        version: Some(version),
        ..named("a.b")
    }
}

fn with_key(key: &str) -> EmitRequest {
    EmitRequest {
        idempotency_key: Some(key.into()),
        ..named("a.b")
    }
}

/// `{"<key>":"aaa…"}` of exactly `size` bytes.
fn blob(key: &str, size: usize) -> String {
    format!(r#"{{"{key}":"{}"}}"#, "a".repeat(size - key.len() - 7))
}

/// Every event rule's refusal, by the start of its message, and the accepted edges (`None`).
fn cases() -> Vec<(EmitRequest, Option<String>)> {
    let refused = |m: &str| Some(m.to_owned());
    let name_bytes = format!("event name exceeds {MAX_EVENT_NAME_BYTES} bytes");
    let key_bytes = format!("idempotency key must be 1 to {MAX_IDEMPOTENCY_KEY_BYTES} bytes");
    vec![
        (named("order.placed"), None),
        (named(&"a".repeat(MAX_EVENT_NAME_BYTES)), None),
        (with_data(blob("b", MAX_DATA_BYTES).as_bytes()), None),
        (with_metadata(&blob("t", MAX_METADATA_BYTES)), None),
        (with_version(i64::from(i32::MAX)), None),
        (with_key(&"k".repeat(MAX_IDEMPOTENCY_KEY_BYTES)), None),
        (named(""), refused("event name is required")),
        (
            named(&"a".repeat(MAX_EVENT_NAME_BYTES + 1)),
            Some(name_bytes),
        ),
        (named("a..b"), refused("event name has an empty segment")),
        (named("a b"), refused("event name contains whitespace")),
        (named("a\u{1}b"), refused("event name contains whitespace")),
        (
            named("ironflowx.run.completed"),
            refused("event name is owned"),
        ),
        (named("$system"), refused("event name is owned")),
        (
            with_data(blob("b", MAX_DATA_BYTES + 1).as_bytes()),
            refused("event data exceeds maximum size"),
        ),
        (with_data(b"{\"k\": 1"), refused("event data is invalid: ")),
        (with_data(b"\xff"), refused("event data is invalid: ")),
        (
            with_data(br#"{"a": 1, "a": 2}"#),
            refused("event data is invalid: repeated key"),
        ),
        (
            with_data(br#""x\u0000y""#),
            refused("event data is invalid: NUL"),
        ),
        (
            with_data(br#"{"$redacted": 1}"#),
            refused("event data is invalid: top-level"),
        ),
        (with_data(b"1e400"), refused("event data is invalid: ")),
        (
            with_data(format!("{}{}", "[".repeat(128), "]".repeat(128)).as_bytes()),
            refused("event data is invalid: recursion limit"),
        ),
        (
            with_metadata(&blob("t", MAX_METADATA_BYTES + 1)),
            refused("event metadata exceeds maximum size"),
        ),
        (with_metadata("{"), refused("event metadata is invalid: ")),
        (
            with_metadata("[1]"),
            refused("event metadata is not a JSON object"),
        ),
        (with_version(0), refused("event version is outside")),
        (
            with_version(i64::from(i32::MAX) + 1),
            refused("event version is outside"),
        ),
        (with_key(""), Some(key_bytes.clone())),
        (
            with_key(&"k".repeat(MAX_IDEMPOTENCY_KEY_BYTES + 1)),
            Some(key_bytes.clone()),
        ),
        (with_key("k\0"), Some(key_bytes)),
    ]
}

#[tokio::test]
async fn check_emit_answers_as_an_emit_does() {
    let dir = tempfile::tempdir().unwrap();
    let clock = ManualClock::new(Timestamp::from_millis(1_800_000_000_000));
    let opened = Engine::open(dir.path(), clock, None).await.unwrap();
    let (engine, cx) = (opened.engine, Call::with_key(opened.minted.unwrap()));

    for (req, expected) in cases() {
        let checked = check_emit(&req).map_err(|e| (e.kind, e.message, e.hint));
        let emitted = engine
            .emit(&cx, req.clone())
            .await
            .map(drop)
            .map_err(|e| (e.kind, e.message, e.hint));
        assert_eq!(checked, emitted, "{:?}", req.name);
        match (&checked, expected) {
            (Ok(()), None) => {}
            (Err((ErrorKind::Invalid, message, None)), Some(start)) => {
                assert!(message.starts_with(&start), "{message:?} for {start:?}");
            }
            (got, expected) => panic!("{got:?} where {expected:?} was expected"),
        }
    }
    engine.close().await;
}
