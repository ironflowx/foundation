//! The proto client, through the HTTP door of a recording `Operations`.

mod common;

use std::collections::BTreeSet;
use std::num::NonZeroU32;
use std::sync::atomic::Ordering::Relaxed;
use std::time::Duration;

use common::{
    CREATED, DATA, KEY, KINDS, METADATA, batch, created, emitted, environments, event, event_page,
    key_page, serving, whoami,
};
use ironflowx_core::{Address, ErrorKind, Scope, Secret, Timestamp};
use ironflowx_proto::{
    Call, Client, CreateKeyRequest, EmitBatchRequest, EmitRequest, GetEventRequest, InvalidUrl,
    ListEnvironmentsRequest, ListEventsRequest, ListKeysRequest, OpError, Operation, Operations,
    RevokeKeyRequest, Whoami, WhoamiRequest,
};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpListener;

fn call(key: &str) -> Call {
    Call::with_key(Secret::new(key.to_owned()))
}

async fn ask(client: &Client, cx: &Call) -> Result<Whoami, OpError> {
    client.whoami(cx, WhoamiRequest {}).await
}

#[tokio::test]
async fn one_client_sends_each_calls_own_key() {
    let (addr, operations) = serving(Ok(whoami())).await;
    let client = Client::new(&format!("http://{addr}")).unwrap();
    for (cx, sent) in [
        (call(KEY), Some(KEY)),
        (Call::anonymous(), None),
        (call("x"), Some("x")),
        (call(KEY), Some(KEY)),
        // The Bearer scheme with no text is no key, as it is in process.
        (call(""), None),
        // Not well formed, and a header would not hold them, or not as they are.
        (call(&format!("{KEY} ")), Some("malformed")),
        (call(&format!(" {KEY}")), Some("malformed")),
        (call(&format!("{KEY}\n")), Some("malformed")),
        (call("\0"), Some("malformed")),
        (call("clé"), Some("malformed")),
        (call("\u{7f}"), Some("malformed")),
        (call(" "), Some("malformed")),
    ] {
        assert_eq!(ask(&client, &cx).await, Ok(whoami()), "{sent:?}");
        assert_eq!(operations.take(), [sent.map(str::to_owned)]);
    }
}

/// SRV-3: the engine's own error, exactly.
#[tokio::test]
async fn every_error_crosses_to_the_client() {
    let (addr, operations) = serving(Ok(whoami())).await;
    let client = Client::new(&format!("http://{addr}")).unwrap();
    for kind in KINDS {
        for hint in [None, Some(String::new()), Some("do this".to_owned())] {
            let sent = OpError {
                kind,
                message: "réfusé".into(),
                hint,
            };
            *operations.answer.lock().unwrap() = Err(sent.clone());
            assert_eq!(ask(&client, &call(KEY)).await, Err(sent));
        }
    }
}

fn item(name: &str) -> EmitRequest {
    EmitRequest {
        name: name.into(),
        data: Some(DATA.into()),
        metadata: Some(METADATA.into()),
        version: Some(7),
        idempotency_key: Some("clé".into()),
    }
}

/// Calls `op` once: what it answered, as it prints.
async fn ask_one(client: &Client, cx: &Call, op: Operation) -> Result<String, OpError> {
    match op {
        Operation::Whoami => ask(client, cx).await.map(|a| format!("{a:?}")),
        Operation::ListEnvironments => {
            let answer = client.list_environments(cx, ListEnvironmentsRequest {});
            answer.await.map(|a| format!("{a:?}"))
        }
        Operation::CreateKey => {
            let request = CreateKeyRequest {
                scopes: [Scope::Write].into(),
                expires_at: Some(Timestamp::from_millis(1_800_000_000_001)),
                label: Some("clé".into()),
            };
            let answer = client.create_key(cx, request).await;
            answer.map(|a| format!("{} {:?}", a.key.expose(), a.info))
        }
        Operation::ListKeys => {
            let request = ListKeysRequest {
                limit: NonZeroU32::new(7),
                cursor: Some("c".into()),
            };
            let answer = client.list_keys(cx, request).await;
            answer.map(|a| format!("{a:?}"))
        }
        Operation::RevokeKey => {
            let request = RevokeKeyRequest {
                key_id: "no ID".into(),
            };
            let answer = client.revoke_key(cx, request).await;
            answer.map(|a| format!("{a:?}"))
        }
        Operation::Emit => client.emit(cx, item("a")).await.map(|a| format!("{a:?}")),
        Operation::EmitBatch => {
            // As many as `batch` answers.
            let items = vec![item("b"); batch().items.len()];
            let answer = client.emit_batch(cx, EmitBatchRequest { items }).await;
            answer.map(|a| format!("{a:?}"))
        }
        Operation::GetEvent => {
            let request = GetEventRequest {
                event_id: "no ID".into(),
            };
            let answer = client.get_event(cx, request).await;
            answer.map(|a| format!("{a:?}"))
        }
        Operation::ListEvents => {
            let request = ListEventsRequest {
                limit: NonZeroU32::new(7),
                cursor: Some("c".into()),
            };
            let answer = client.list_events(cx, request).await;
            answer.map(|a| format!("{a:?}"))
        }
    }
}

/// SRV-1, SRV-3: each request arrives as it was asked, once, with its call's key; the answer, or
/// the error, comes back unchanged.
#[tokio::test]
async fn each_operation_crosses_to_the_engine_and_back() {
    let (addr, operations) = serving(Ok(whoami())).await;
    let client = Client::new(&format!("http://{addr}")).unwrap();
    let answers = [
        format!("{:?}", emitted()),
        format!("{:?}", batch()),
        format!("{:?}", event()),
        format!("{:?}", event_page()),
        format!("{:?}", whoami()),
        format!("{:?}", environments()),
        format!("{CREATED} {:?}", created().info),
        format!("{:?}", key_page()),
        "()".to_owned(),
    ];
    let mut asked = Vec::new();
    for (op, answer) in Operation::ALL.iter().zip(answers) {
        let got = ask_one(&client, &call(KEY), *op).await;
        assert_eq!(got, Ok(answer), "{op:?}");
        assert_eq!(operations.take(), [Some(KEY.to_owned())], "{op:?}");
        asked.extend(operations.take_requests());
    }
    let items = vec![item("b"); batch().items.len()];
    let sent = [
        format!("{:?}", item("a")),
        format!("{:?}", EmitBatchRequest { items }),
        r#"GetEventRequest { event_id: "no ID" }"#.to_owned(),
        r#"ListEventsRequest { limit: Some(7), cursor: Some("c") }"#.to_owned(),
        "WhoamiRequest".to_owned(),
        "ListEnvironmentsRequest".to_owned(),
        r#"CreateKeyRequest { scopes: {Write}, expires_at: Some(Timestamp { millis: 1800000000001 }), label: Some("clé") }"#.to_owned(),
        r#"ListKeysRequest { limit: Some(7), cursor: Some("c") }"#.to_owned(),
        r#"RevokeKeyRequest { key_id: "no ID" }"#.to_owned(),
    ];
    assert_eq!(asked, sent);

    let e = OpError {
        kind: ErrorKind::NotFound,
        message: "m".into(),
        hint: Some("h".into()),
    };
    *operations.answer.lock().unwrap() = Err(e.clone());
    for op in Operation::ALL {
        let answer = ask_one(&client, &call(KEY), *op).await;
        assert_eq!(answer, Err(e.clone()), "{op:?}");
    }
    assert_eq!(operations.take().len(), 9);
}

/// D74: bytes that are no text are not sent, and one such item keeps its whole batch back.
#[tokio::test]
async fn data_that_is_not_utf8_is_not_sent() {
    let (addr, operations) = serving(Ok(whoami())).await;
    let client = Client::new(&format!("http://{addr}")).unwrap();
    let bad = b"\"\xff\xfe\"".to_vec();
    let data = EmitRequest {
        data: Some(bad.clone()),
        ..item("a")
    };
    let metadata = EmitRequest {
        metadata: Some(bad),
        ..item("a")
    };
    for (bad, message) in [
        (data, "event data is not UTF-8"),
        (metadata, "event metadata is not UTF-8"),
    ] {
        let refused = OpError {
            kind: ErrorKind::Invalid,
            message: message.into(),
            hint: None,
        };
        let e = client.emit(&call(KEY), bad.clone()).await.unwrap_err();
        assert_eq!(e, refused);
        for items in [
            vec![bad.clone()],
            vec![bad.clone(), item("a")],
            vec![item("a"), bad],
        ] {
            let batch = EmitBatchRequest { items };
            let e = client.emit_batch(&call(KEY), batch).await.unwrap_err();
            assert_eq!(e, refused);
        }
    }
    assert_eq!(operations.take(), []);
}

/// What became of some item is then not known.
#[tokio::test]
async fn a_batch_answered_with_another_number_of_results_is_unknown() {
    let (addr, _operations) = serving(Ok(whoami())).await;
    let client = Client::new(&format!("http://{addr}")).unwrap();
    let n = batch().items.len();
    for items in [1, n - 1, n + 1] {
        let items = vec![item("a"); items];
        let batch = EmitBatchRequest { items };
        let e = client.emit_batch(&call(KEY), batch).await.unwrap_err();
        let unknown = OpError {
            kind: ErrorKind::Unknown,
            message: "the engine did not answer: unknown".into(),
            hint: None,
        };
        assert_eq!(e, unknown);
    }
}

/// The wire does not carry the expiry, and the nearest it does is not the one asked for.
#[tokio::test]
async fn an_expiry_outside_the_wires_years_is_not_sent() {
    let (addr, operations) = serving(Ok(whoami())).await;
    let client = Client::new(&format!("http://{addr}")).unwrap();
    for outside in [-62_135_596_800_001, 253_402_300_800_000] {
        let request = CreateKeyRequest {
            expires_at: Some(Timestamp::from_millis(outside)),
            ..Default::default()
        };
        let e = client.create_key(&call(KEY), request).await.unwrap_err();
        let refused = OpError {
            kind: ErrorKind::Invalid,
            message: "a time is in whole milliseconds and in years 0001 to 9999".into(),
            hint: None,
        };
        assert_eq!(e, refused, "{outside}");
    }
    assert_eq!(operations.take(), []);
}

#[tokio::test]
async fn an_address_prints_back_exactly() {
    let (addr, operations) = serving(Ok(whoami())).await;
    let client = Client::new(&format!("http://{addr}/")).unwrap();
    for (project, environment, split) in [
        ("default", "main", ("default", "main")),
        ("a", "b/c", ("a", "b/c")),
        ("a/b", "c", ("a", "b/c")),
        ("", "", ("", "")),
    ] {
        let sent = Whoami {
            environment: Address::new(project, environment),
            expires_at: None,
            scopes: BTreeSet::new(),
            ..whoami()
        };
        *operations.answer.lock().unwrap() = Ok(sent.clone());
        let got = ask(&client, &call(KEY)).await.unwrap();
        let text = format!("{project}/{environment}");
        assert_eq!(got.environment.to_string(), text);
        assert_eq!(got.environment, Address::new(split.0, split.1), "{text}");
        assert_eq!((got.scopes, got.expires_at), (sent.scopes, None));
    }
}

#[tokio::test]
async fn an_answer_holds_at_most_16_mib() {
    let (addr, operations) = serving(Ok(whoami())).await;
    let client = Client::new(&format!("http://{addr}")).unwrap();
    // The message's other fields, and each field's tag and length, around the project's name.
    let sized = |size: usize| Whoami {
        environment: Address::new(&"a".repeat(size - 38 - 5 - 2), "b"),
        expires_at: None,
        scopes: BTreeSet::new(),
        ..whoami()
    };
    let largest = sized(16 * 1024 * 1024);
    *operations.answer.lock().unwrap() = Ok(largest.clone());
    assert!(ask(&client, &call(KEY)).await == Ok(largest));
    *operations.answer.lock().unwrap() = Ok(sized(16 * 1024 * 1024 + 1));
    let refused = ask(&client, &call(KEY)).await.unwrap_err();
    assert_eq!((refused.kind, refused.hint), (ErrorKind::Unavailable, None));
}

/// Answers one request with `answer`, then closes.
async fn stub(answer: Vec<u8>) -> Client {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = [0; 4096];
        let _ = stream.read(&mut request).await;
        stream.write_all(&answer).await.unwrap();
    });
    Client::new(&format!("http://{addr}")).unwrap()
}

/// What a call without the engine's own error ends as.
fn unanswered(op: Operation) -> (ErrorKind, Option<&'static str>) {
    match op {
        Operation::CreateKey => (
            ErrorKind::Unknown,
            Some(
                "the key may be stored: list the keys before the retry, and again after the \
                 engine reopens; revoke the one no holder has",
            ),
        ),
        Operation::RevokeKey => (
            ErrorKind::Unknown,
            Some("retry: revoking a revoked key succeeds, and one that revoked itself is refused"),
        ),
        Operation::Emit | Operation::EmitBatch => (ErrorKind::Unknown, None),
        _ => (ErrorKind::Unavailable, None),
    }
}

/// No answer, or one that is not the engine's own: a read is `Unavailable`, a state change
/// `Unknown`, and no message holds a key (TEN-3) or what can be an event's data.
#[tokio::test]
async fn a_call_no_engine_answers_says_whether_it_may_have_run() {
    let closed = {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        format!("http://{}", listener.local_addr().unwrap())
    };
    let http = |status: &str, kind: &str, body: &[u8]| {
        let len = body.len();
        let head =
            format!("HTTP/1.1 {status}\r\ncontent-type: {kind}\r\ncontent-length: {len}\r\n\r\n");
        [head.as_bytes(), body].concat()
    };
    // A CreateKeyResponse's key, then no information, or some cut short.
    let key = [b"\n\x54", CREATED.as_bytes()].concat();
    let message = format!(r#"{{"code":"unavailable","message":"{CREATED}"}}"#).into_bytes();
    let cut = [&key, b"\x12\x7fx".as_slice()].concat();
    // A WhoamiResponse or a CreateKeyResponse whose first field is "x"; as a page, one that holds
    // a message that does not decode.
    let short = b"\n\x01x";
    let answers = [
        // Holds nothing, as a revoke's answer does, and a page with nothing.
        http("200 OK", "application/proto", b""),
        // As that, but none of Connect's.
        http("204 No Content", "application/proto", b""),
        http("202 Accepted", "application/proto", b""),
        b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n".to_vec(),
        http("200 OK", "application/protobuf", b""),
        http("200 OK", "application/proto, text/html", b""),
        // A created key, in the body of an error that is not the engine's.
        http("502 Bad Gateway", "application/proto", &key),
        http("502 Bad Gateway", "application/json", &message),
        [b"HTTP/1.1 502 Bad Gateway\r\n\r\n", &*key].concat(),
        vec![],
        http("401 Unauthorized", "text/html", b"<html>"),
        http("503 Service Unavailable", "text/html", b"<html>"),
        http("200 OK", "text/html", b""),
        http("200 OK", "application/proto", b"\x7f"),
        http("200 OK", "application/proto", short),
        http("200 OK", "application/proto", &key),
        http("200 OK", "application/proto", &cut),
        // ErrorInfo of a kind this client does not know: 'kind' "new".
        http(
            "400 Bad Request",
            "application/json",
            br#"{"code":"invalid_argument","details":[{"type":"ironflowx.v1.ErrorInfo","value":"CgNuZXc"}]}"#,
        ),
    ];
    let ignored = [Some(&answers[14]), Some(&answers[15])];
    let nothing = Some(&answers[0]);
    for op in Operation::ALL.iter().copied() {
        for answer in std::iter::once(None).chain(answers.iter().map(Some)) {
            let client = match answer {
                None => Client::new(&closed).unwrap(),
                Some(answer) => stub(answer.clone()).await,
            };
            let shown = answer.map(|a| a.escape_ascii().to_string());
            let got = ask_one(&client, &call(KEY), op).await;
            // A revoke's answer holds nothing, so another message is one with fields it ignores.
            if op == Operation::RevokeKey && ignored.contains(&answer) {
                assert_eq!(got, Ok("()".to_owned()), "{shown:?}");
                continue;
            }
            let empty = match op {
                Operation::RevokeKey => Some("()"),
                Operation::ListKeys => Some("KeyPage { keys: [], next: None }"),
                Operation::ListEvents => Some("EventPage { events: [], next: None }"),
                Operation::ListEnvironments => Some("[]"),
                _ => None,
            };
            if answer == nothing
                && let Some(empty) = empty
            {
                assert_eq!(got, Ok(empty.to_owned()));
                continue;
            }
            let e = got.unwrap_err();
            let ended = (e.kind, e.hint.as_deref());
            assert_eq!(ended, unanswered(op), "{op:?} {shown:?}");
            assert!(!e.message.contains(&KEY[41..]), "{}", e.message);
            let held = op == Operation::CreateKey || op.rpc().contains("EventService");
            if held {
                assert!(!e.message.contains(&CREATED[41..62]), "{}", e.message);
                assert!(!e.message.contains("html"), "{}", e.message);
            }
        }
    }
}

/// The client ends a call, whatever the engine does, and tells it when.
#[tokio::test]
async fn a_call_ends_by_its_timeout() {
    let late = Duration::from_secs(30);
    let (addr, operations) = serving(Ok(whoami())).await;
    operations.hangs.store(true, Relaxed);
    // One that never answers, and records what it was asked.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let silent = listener.local_addr().unwrap();
    let asked = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut head = Vec::new();
        while !head.windows(4).any(|w| w == b"\r\n\r\n") {
            assert_ne!(stream.read_buf(&mut head).await.unwrap(), 0);
        }
        (stream, String::from_utf8_lossy(&head).to_lowercase())
    });
    for addr in [addr, silent] {
        let client = Client::new(&format!("http://{addr}")).unwrap();
        let client = client.with_timeout(Duration::from_millis(250));
        for op in Operation::ALL.iter().copied() {
            let ended = tokio::time::timeout(late, ask_one(&client, &call(KEY), op)).await;
            let e = ended.expect("in time").unwrap_err();
            assert_eq!((e.kind, e.hint.as_deref()), unanswered(op), "{op:?}");
        }
    }
    let asked = tokio::time::timeout(late, asked).await;
    let (_stream, head) = asked.expect("a call").unwrap();
    assert!(head.contains("\r\nconnect-timeout-ms: 250\r\n"), "{head}");
}

#[test]
fn a_url_is_http_with_a_host_and_no_user_query_or_fragment() {
    for url in [
        "http://127.0.0.1:9310",
        "http://127.0.0.1:9310/",
        "http://localhost",
        "http://[::1]:9310",
        "http://proxy.example/ironflow",
        "http://h:65535",
        "http://h:0080",
        "http://[::ffff:127.0.0.1]",
    ] {
        assert!(Client::new(url).is_ok(), "{url}");
    }
    for url in [
        "",
        "127.0.0.1:9310",
        "/path",
        "http://",
        "http:///path",
        "http://:80",
        "https://127.0.0.1:9310",
        "ftp://127.0.0.1",
        "unix:/tmp/x",
        "http://user@h",
        "http://user:pass@h",
        "http://h?token=x",
        "http://h/?",
        "http://h/#x",
        "http://h#x",
        "http://h:",
        "http://h:65536",
        "http://h:x",
        "http://h:+80",
        "http://h:-1",
        "http://[bad]:80",
        "http://[::1",
        "http://[::1]x",
        "http://[]",
        "http://[::1]]",
        "http://[fe80::1%25en0]",
        "http://u@[::1]:80",
        "http://h@h",
        "http://h@h:80",
        "http://host-user:pw@host:80/",
        "http://[::1]x:80",
        "http://h:80:80",
        "http:// h",
        "http://h /",
    ] {
        assert_eq!(Client::new(url).err(), Some(InvalidUrl), "{url}");
    }
}
