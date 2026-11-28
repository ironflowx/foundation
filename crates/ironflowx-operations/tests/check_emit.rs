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

/// What an emit answers: accepted, refused with exactly this message, or refused with a message
/// that starts so, where `serde_json` writes the rest (a line and a column).
#[derive(Debug)]
enum Expect {
    Accepted,
    Is(String),
    Starts(&'static str),
}

/// Every event rule's refusal, and the accepted edges.
fn cases() -> Vec<(EmitRequest, Expect)> {
    use Expect::{Accepted, Starts};
    let is = |m: &str| Expect::Is(m.to_owned());
    let name_bytes = format!("event name exceeds {MAX_EVENT_NAME_BYTES} bytes");
    let key_bytes =
        format!("idempotency key must be 1 to {MAX_IDEMPOTENCY_KEY_BYTES} bytes with no NUL");
    let control = "event name contains whitespace or a control character";
    let owned = "event name is owned by the engine";
    let version = "event version is outside 1 to 2147483647";
    vec![
        (named("order.placed"), Accepted),
        (named(&"a".repeat(MAX_EVENT_NAME_BYTES)), Accepted),
        (with_data(blob("b", MAX_DATA_BYTES).as_bytes()), Accepted),
        (with_metadata(&blob("t", MAX_METADATA_BYTES)), Accepted),
        (with_version(i64::from(i32::MAX)), Accepted),
        (with_key(&"k".repeat(MAX_IDEMPOTENCY_KEY_BYTES)), Accepted),
        (named(""), is("event name is required")),
        (
            named(&"a".repeat(MAX_EVENT_NAME_BYTES + 1)),
            is(&name_bytes),
        ),
        (named("a..b"), is("event name has an empty segment")),
        (named("a b"), is(control)),
        (named("a\u{1}b"), is(control)),
        (named("ironflowx.run.completed"), is(owned)),
        (named("$system"), is(owned)),
        (
            with_data(blob("b", MAX_DATA_BYTES + 1).as_bytes()),
            is("event data exceeds maximum size"),
        ),
        (
            with_data(b"{\"k\": 1"),
            Starts("event data is invalid: EOF while parsing"),
        ),
        (
            with_data(b"\"\xff\""),
            Starts("event data is invalid: invalid unicode"),
        ),
        (
            with_data(br#"{"a": 1, "a": 2}"#),
            Starts("event data is invalid: repeated key at"),
        ),
        (
            with_data(br#""x\u0000y""#),
            Starts("event data is invalid: NUL in a key or string at"),
        ),
        (
            with_data(br#"{"$redacted": 1}"#),
            Starts("event data is invalid: top-level $redacted key at"),
        ),
        (
            with_data(b"1e400"),
            Starts("event data is invalid: number out of range at"),
        ),
        (
            with_data(format!("{}{}", "[".repeat(128), "]".repeat(128)).as_bytes()),
            Starts("event data is invalid: recursion limit exceeded at"),
        ),
        (
            with_metadata(&blob("t", MAX_METADATA_BYTES + 1)),
            is("event metadata exceeds maximum size"),
        ),
        (
            with_metadata("{"),
            Starts("event metadata is invalid: EOF while parsing"),
        ),
        (
            with_metadata("[1]"),
            is("event metadata is not a JSON object"),
        ),
        (with_version(0), is(version)),
        (with_version(i64::from(i32::MAX) + 1), is(version)),
        (with_key(""), is(&key_bytes)),
        (
            with_key(&"k".repeat(MAX_IDEMPOTENCY_KEY_BYTES + 1)),
            is(&key_bytes),
        ),
        (with_key("k\0"), is(&key_bytes)),
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
        let met = match (&checked, &expected) {
            (Ok(()), Expect::Accepted) => true,
            (Err((ErrorKind::Invalid, message, None)), Expect::Is(want)) => message == want,
            (Err((ErrorKind::Invalid, message, None)), Expect::Starts(want)) => {
                message.starts_with(want)
            }
            _ => false,
        };
        assert!(met, "{checked:?} where {expected:?} was expected");
    }
    engine.close().await;
}
