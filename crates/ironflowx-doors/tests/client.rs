//! The proto client, through the HTTP door of a recording `Operations`.

mod common;

use std::collections::BTreeSet;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;
use std::time::Duration;

use common::{
    CREATED, DATA, KEY, KINDS, METADATA, batch, created, emitted, environments, event, event_page,
    key_page, serve, serving, whoami,
};
use ironflowx_core::{Address, ErrorKind, Scope, Secret, Timestamp};
use ironflowx_doors::Slot;
use ironflowx_proto::{
    Call, Client, CreateKeyRequest, EmitBatchRequest, EmitRequest, GetEventRequest,
    ListEnvironmentsRequest, ListEventsRequest, ListKeysRequest, OpError, Operation, Operations,
    RevokeKeyRequest, Whoami, WhoamiRequest,
};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

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

/// While the engine opens.
#[tokio::test]
async fn each_operation_answers_that_the_engine_is_starting() {
    let addr = serve(Arc::new(Slot::starting())).await;
    let client = Client::new(&format!("http://{addr}")).unwrap();
    for op in Operation::ALL {
        let starting = OpError {
            kind: ErrorKind::Unavailable,
            message: "the engine is unavailable".into(),
            hint: Some("the engine is starting".into()),
        };
        let answer = ask_one(&client, &call(KEY), *op).await;
        assert_eq!(answer, Err(starting), "{op:?}");
    }
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
            hint: unanswered(Operation::EmitBatch).1.map(str::to_owned),
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

/// The Errors table: it is not sent, so its answer is never lost with the connection.
#[tokio::test]
async fn a_request_over_16_mib_is_not_sent() {
    let (addr, operations) = serving(Ok(whoami())).await;
    let client = Client::new(&format!("http://{addr}")).unwrap();
    // The fields of each message, their tags and lengths, around what fills it.
    let emit = |size: usize| EmitRequest {
        name: "a".into(),
        data: Some(vec![b'1'; size - 3 - 5]),
        metadata: None,
        version: None,
        idempotency_key: None,
    };
    let text = |size: usize| "x".repeat(size - 5);
    let get = |size: usize| GetEventRequest {
        event_id: text(size),
    };
    let revoke = |size: usize| RevokeKeyRequest { key_id: text(size) };
    let create = |size: usize| CreateKeyRequest {
        label: Some(text(size)),
        ..Default::default()
    };
    let events = |size: usize| ListEventsRequest {
        limit: None,
        cursor: Some(text(size)),
    };
    let keys = |size: usize| ListKeysRequest {
        limit: None,
        cursor: Some(text(size)),
    };
    let most = 16 * 1024 * 1024;
    let cx = call(KEY);
    assert_eq!(client.emit(&cx, emit(most)).await, Ok(emitted()));
    let items = |size: usize| EmitBatchRequest {
        items: vec![emit(size - 5)],
    };
    // A batch of one item, which the engine here answers as another: its count is not the one sent.
    let sent = client.emit_batch(&cx, items(most)).await;
    assert_eq!(sent.unwrap_err().kind, ErrorKind::Unknown);
    assert_eq!(client.get_event(&cx, get(most)).await, Ok(event()));
    let sent = client.list_events(&cx, events(most)).await;
    assert_eq!(sent, Ok(event_page()));
    let sent = client.create_key(&cx, create(most)).await.unwrap();
    assert_eq!(sent.info, created().info);
    assert_eq!(client.list_keys(&cx, keys(most)).await, Ok(key_page()));
    assert_eq!(client.revoke_key(&cx, revoke(most)).await, Ok(()));
    assert_eq!(operations.take().len(), 7);

    let over = most + 1;
    let too_large = || OpError {
        kind: ErrorKind::ResourceExhausted,
        message: "the request is over 16 MiB".into(),
        hint: None,
    };
    for refused in [
        client.emit(&cx, emit(over)).await.unwrap_err(),
        client.emit_batch(&cx, items(over)).await.unwrap_err(),
        client.get_event(&cx, get(over)).await.unwrap_err(),
        client.list_events(&cx, events(over)).await.unwrap_err(),
        client.create_key(&cx, create(over)).await.unwrap_err(),
        client.list_keys(&cx, keys(over)).await.unwrap_err(),
        client.revoke_key(&cx, revoke(over)).await.unwrap_err(),
    ] {
        assert_eq!(refused, too_large());
    }
    // As one too large for protobuf: 2 GiB less a byte, and its field's tag and length.
    let huge = RevokeKeyRequest {
        key_id: "x".repeat(i32::MAX as usize),
    };
    let refused = client.revoke_key(&cx, huge).await.unwrap_err();
    assert_eq!(refused, too_large());
    // Nor did it try: no door is there.
    let client = closed().await;
    let refused = client.emit(&cx, emit(over)).await.unwrap_err();
    assert_eq!(refused, too_large());
    assert_eq!(operations.take(), []);
}

/// A client of a port no one listens on.
async fn closed() -> Client {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    Client::new(&format!("http://{}", listener.local_addr().unwrap())).unwrap()
}

/// A server of one port, whose connections leave no other in use.
struct Stub(Arc<TcpListener>);

impl Stub {
    async fn new() -> Self {
        Self(Arc::new(TcpListener::bind("127.0.0.1:0").await.unwrap()))
    }

    /// A client whose one request is answered with `answer`; then the connection closes.
    fn answering(&self, answer: Vec<u8>) -> Client {
        let listener = self.0.clone();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            let _ = stream.read(&mut request).await;
            stream.write_all(&answer).await.unwrap();
        });
        Client::new(&format!("http://{}", self.0.local_addr().unwrap())).unwrap()
    }
}

/// What a call that may have run ends as, where no engine answered it. Every item sent here has a
/// key.
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
        Operation::Emit => (
            ErrorKind::Unknown,
            Some("retry with the same idempotency key"),
        ),
        Operation::EmitBatch => (
            ErrorKind::Unknown,
            Some(
                "retry unchanged only if every item had an idempotency key and keeps it; \
                 else check first what landed",
            ),
        ),
        _ => (ErrorKind::Unavailable, None),
    }
}

fn http(status: &str, kind: &str, body: &[u8]) -> Vec<u8> {
    let len = body.len();
    let head =
        format!("HTTP/1.1 {status}\r\ncontent-type: {kind}\r\ncontent-length: {len}\r\n\r\n");
    [head.as_bytes(), body].concat()
}

fn json(status: &str, body: &str) -> Vec<u8> {
    http(status, "application/json", body.as_bytes())
}

/// SRV-4, rule 3: no connection opened, so nothing ran, and any call may be sent again.
#[tokio::test]
async fn a_call_that_never_connects_is_unavailable() {
    let client = closed().await;
    for op in Operation::ALL.iter().copied() {
        let e = ask_one(&client, &call(KEY), op).await.unwrap_err();
        let ended = (e.kind, e.hint);
        assert_eq!(ended, (ErrorKind::Unavailable, None), "{op:?}");
        assert_eq!(e.message, "the engine did not answer: unavailable");
    }
}

/// SRV-4, rule 2: no answer, or one that is not the engine's own: a read is `Unavailable`, a state
/// change `Unknown`, and no message holds anything of a body, which can hold a key (TEN-3) or an
/// event's data. One made from an HTTP status names it.
#[tokio::test]
async fn a_call_no_engine_answers_says_whether_it_may_have_run() {
    let stub = Stub::new().await;
    // A CreateKeyResponse's key, then no information, or some cut short.
    let key = [b"\n\x54", CREATED.as_bytes()].concat();
    let message = format!(r#"{{"code":"unavailable","message":"{CREATED}"}}"#);
    let cut = [&key, b"\x12\x7fx".as_slice()].concat();
    // A WhoamiResponse or a CreateKeyResponse whose first field is "x"; as a page, one that holds
    // a message that does not decode.
    let short = b"\n\x01x";
    let chunked = b"HTTP/1.1 502 Bad Gateway\r\n\r\n";
    let gzip = b"HTTP/1.1 200 OK\r\ncontent-type: application/proto\r\ncontent-encoding: gzip\r\n\
                 content-length: 1\r\n\r\nx";
    let refusal = br#"{"code":"invalid_argument"}"#.as_slice();
    let head = |head: &str, body: &[u8]| {
        let head = format!(
            "HTTP/1.1 400 x\r\n{head}content-length: {}\r\n\r\n",
            body.len()
        );
        [head.as_bytes(), body].concat()
    };
    let unfinished = b"HTTP/1.1 501 x\r\ncontent-type: application/json\r\ncontent-length: 99\r\n\r\n{\"code\":\"unimplemented\"}";
    // ErrorInfo of a kind this client does not know: 'kind' "new".
    let new_kind = r#"{"code":"invalid_argument","details":[{"type":"ironflowx.v1.ErrorInfo","value":"CgNuZXc"}]}"#;
    let unreadable =
        r#"{"code":"unimplemented","details":[{"type":"ironflowx.v1.ErrorInfo","value":"!"}]}"#;
    let another = r#"{"code":"resource_exhausted","details":[{"type":"x"}]}"#;
    let own_field = |text: &[u8]| {
        let before =
            br#"{"details":[{"type":"ironflowx.v1.ErrorInfo","value":"Cglub3RfZm91bmQSAW0","x":""#;
        http(
            "502 x",
            "application/json",
            &[before, text, br#""}]}"#].concat(),
        )
    };
    // Each with the HTTP status its message names, or the Connect code of the failure it is.
    let status = |n: u16| format!("an HTTP {n} that is not Connect's");
    let code = str::to_owned;
    let answers = [
        // Holds nothing, as a revoke's answer does, and a page with nothing.
        (code("unknown"), http("200 OK", "application/proto", b"")),
        (code("internal"), http("200 OK", "application/proto", short)),
        (code("internal"), http("200 OK", "application/proto", &key)),
        // As the first, but none of Connect's.
        (
            status(204),
            http("204 No Content", "application/proto", b""),
        ),
        (status(202), http("202 Accepted", "application/proto", b"")),
        (
            status(200),
            b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n".to_vec(),
        ),
        (status(200), http("200 OK", "application/protobuf", b"")),
        (
            status(200),
            http("200 OK", "application/proto, text/html", b""),
        ),
        (status(200), http("200 OK", "text/html", b"")),
        // A created key, in the body of an error that is not the engine's.
        (
            status(502),
            http("502 Bad Gateway", "application/proto", &key),
        ),
        (status(502), json("502 Bad Gateway", &message)),
        (code("unavailable"), json("503 x", &message)),
        (status(502), [chunked.as_slice(), &key].concat()),
        // A proxy's refusals, whatever Connect code the library has for the status.
        (status(301), http("301 Moved Permanently", "text/html", b"")),
        (status(400), http("400 Bad Request", "text/html", b"")),
        (
            status(401),
            http("401 Unauthorized", "text/html", b"<html>"),
        ),
        (status(403), http("403 Forbidden", "text/html", b"<html>")),
        (status(404), http("404 Not Found", "text/html", b"<html>")),
        (status(413), http("413 Payload Too Large", "text/html", b"")),
        (status(429), http("429 Too Many Requests", "text/html", b"")),
        (status(501), http("501 Not Implemented", "text/html", b"")),
        (
            status(503),
            http("503 Service Unavailable", "text/html", b"<html>"),
        ),
        // JSON that names no code of Connect's.
        (
            status(404),
            json("404 Not Found", r#"{"error":"not found"}"#),
        ),
        (
            status(404),
            json("404 Not Found", r#"{"code":404,"message":"x"}"#),
        ),
        (
            status(404),
            json("404 Not Found", r#"{"code":"Unimplemented"}"#),
        ),
        (status(404), json("404 Not Found", r#"["unimplemented"]"#)),
        (status(400), json("400 Bad Request", "{}")),
        (status(501), unfinished.to_vec()),
        (
            status(400),
            json(
                "400 Bad Request",
                r#"{"code":"new","details":[{"type":"x"}]}"#,
            ),
        ),
        // A code that is not as a server of Connect's sends it.
        (
            status(503),
            json("503 x", r#"{"code":"failed_precondition"}"#),
        ),
        (status(429), json("429 x", r#"{"code":"unimplemented"}"#)),
        (status(400), json("400 x", r#"["invalid_argument","",[]]"#)),
        (status(400), http("400 x", "text/html", refusal)),
        (
            status(400),
            json("400 x", r#"{"code":"invalid_argument","message":{}}"#),
        ),
        (
            status(400),
            json(
                "400 x",
                r#"{"code":"invalid_argument","message":"","message":""}"#,
            ),
        ),
        (
            status(400),
            head(
                "content-type: application/json\r\ncontent-type: text/html\r\n",
                refusal,
            ),
        ),
        (
            status(400),
            http(
                "400 x",
                "application/json; charset=utf-8, text/html",
                refusal,
            ),
        ),
        (
            status(400),
            http("400 x", "application/json; charset=latin1", refusal),
        ),
        (
            status(400),
            http("400 x", "application/jsonx; charset=utf-8", refusal),
        ),
        (
            status(400),
            http("400 x", "application/connect+json", refusal),
        ),
        (
            status(400),
            http("400 x", "application/json, text/html", refusal),
        ),
        (status(400), head("", refusal)),
        (
            status(400),
            head(
                "content-type: text/html\r\ncontent-type: application/json\r\n",
                refusal,
            ),
        ),
        (
            status(400),
            head(
                "content-type: application/json\r\ncontent-encoding: gzip\r\n",
                refusal,
            ),
        ),
        (
            status(400),
            json("400 x", r#"{"code":"invalid_argument","message":null}"#),
        ),
        (
            status(400),
            json("400 x", r#"{"code":"invalid_argument","x":1}"#),
        ),
        // Details that are none, of an error that would be the library's without them.
        (
            status(400),
            json("400 x", r#"{"code":"invalid_argument","details":null}"#),
        ),
        (
            status(400),
            json("400 x", r#"{"code":"invalid_argument","details":{}}"#),
        ),
        (
            status(400),
            json(
                "400 x",
                r#"{"code":"invalid_argument","details":[],"details":[]}"#,
            ),
        ),
        (
            code("invalid_argument"),
            json("400 x", r#"{"code":"invalid_argument","details":[{}]}"#),
        ),
        (
            code("invalid_argument"),
            json(
                "400 x",
                r#"{"code":"invalid_argument","details":[{"type":"x","type":"x"}]}"#,
            ),
        ),
        (
            status(400),
            json("400 x", r#"{"code":["invalid_argument"]}"#),
        ),
        // The engine's error, of a kind this client knows, 'kind' "not_found", where it is not sent
        // as the engine sends it: an array, a field of a proxy's own, after one of no known kind,
        // and printed over lines.
        (
            status(502),
            json(
                "502 x",
                r#"{"details":[["ironflowx.v1.ErrorInfo","Cglub3RfZm91bmQSAW0",null]]}"#,
            ),
        ),
        (
            code("not_found"),
            json(
                "404 x",
                r#"{"code":"not_found","details":[["ironflowx.v1.ErrorInfo","Cglub3RfZm91bmQSAW0",null]]}"#,
            ),
        ),
        (
            status(404),
            json(
                "404 x",
                r#"{"code":"not_found","details":[{"type":"ironflowx.v1.ErrorInfo","value":"Cglub3RfZm91bmQSAW0"}],"trace_id":"x"}"#,
            ),
        ),
        (
            code("not_found"),
            json(
                "404 x",
                r#"{"code":"not_found","details":[{"type":"ironflowx.v1.ErrorInfo","value":"CgNuZXc"},{"type":"ironflowx.v1.ErrorInfo","value":"Cglub3RfZm91bmQSAW0"}]}"#,
            ),
        ),
        (
            status(404),
            json(
                "404 x",
                "{\n\"code\":\"not_found\",\"details\":[{\"type\":\"ironflowx.v1.ErrorInfo\",\"value\":\"Cglub3RfZm91bmQSAW0\"}]}",
            ),
        ),
        // Not from its first byte.
        (
            status(400),
            json("400 x", "\n{\"code\":\"invalid_argument\"}"),
        ),
        (
            status(400),
            json("400 x", " {\"code\":\"invalid_argument\"}"),
        ),
        // Cut off, and one that the library cannot read.
        (code("unavailable"), vec![]),
        (
            code("internal"),
            http("200 OK", "application/proto", b"\x7f"),
        ),
        (code("internal"), http("200 OK", "application/proto", &cut)),
        (code("internal"), gzip.to_vec()),
        // Not the library's, which attaches no detail: the engine's error, which says nothing here.
        (code("invalid_argument"), json("400 Bad Request", new_kind)),
        (code("unimplemented"), json("404 x", unreadable)),
        (code("resource_exhausted"), json("429 x", another)),
        // Nor beside a code or a message that is no text, nor on two lines by any byte of them.
        (
            status(502),
            json(
                "502 x",
                r#"{"code":1,"details":[{"type":"ironflowx.v1.ErrorInfo","value":"Cglub3RfZm91bmQSAW0"}]}"#,
            ),
        ),
        (
            status(502),
            json(
                "502 x",
                r#"{"message":{},"details":[{"type":"ironflowx.v1.ErrorInfo","value":"Cglub3RfZm91bmQSAW0"}]}"#,
            ),
        ),
        (
            status(502),
            json(
                "502 x",
                "{\t\"details\":[{\"type\":\"ironflowx.v1.ErrorInfo\",\"value\":\"Cglub3RfZm91bmQSAW0\"}]}",
            ),
        ),
        (
            status(502),
            json(
                "502 x",
                "{\r\"details\":[{\"type\":\"ironflowx.v1.ErrorInfo\",\"value\":\"Cglub3RfZm91bmQSAW0\"}]}",
            ),
        ),
        // Of a kind this client knows, beside a text that is not UTF-8: no such byte, one written
        // too long, and half a pair.
        (status(502), own_field(b"\xff")),
        (status(502), own_field(b"\xc0\xaf")),
        (status(502), own_field(b"\xed\xa0\x80")),
        // Says in words that no connection opened.
        (
            code("unavailable"),
            json(
                "503 x",
                r#"{"code":"unavailable","message":"HTTP request failed: client error (Connect)"}"#,
            ),
        ),
    ];
    // Decode as messages, which are no answers where an answer needs something.
    let decoded = [&answers[0], &answers[1], &answers[2]];
    let ignored = [&answers[1], &answers[2]];
    for op in Operation::ALL.iter().copied() {
        for answer in &answers {
            let client = stub.answering(answer.1.clone());
            let shown = answer.1.escape_ascii().to_string();
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
            if *answer == answers[0]
                && let Some(empty) = empty
            {
                assert_eq!(got, Ok(empty.to_owned()));
                continue;
            }
            let e = got.unwrap_err();
            let ended = (e.kind, e.hint.as_deref());
            let why = &answer.0;
            assert_eq!(ended, unanswered(op), "{op:?} {shown:?}");
            let one = !matches!(op, Operation::EmitBatch) && !op.rpc().contains("/List");
            let message = match decoded.contains(&answer) && one {
                true => "the answer does not decode".to_owned(),
                false => format!("the engine did not answer: {why}"),
            };
            assert_eq!(e.message, message, "{op:?} {shown:?}");
        }
    }
}

/// SRV-4: an answer, or a proxy's error, that is more than the client takes is no refusal of the
/// request: the library's own error for both is `resource_exhausted`.
#[tokio::test]
async fn an_answer_over_16_mib_is_no_refusal() {
    let stub = Stub::new().await;
    let error = |size: usize| {
        let (start, end) = (r#"{"code":"resource_exhausted","message":""#, r#""}"#);
        let fill = "x".repeat(size - start.len() - end.len());
        json("429 x", &format!("{start}{fill}{end}"))
    };
    let most = 16 * 1024 * 1024;
    let body = vec![0; most + 1];
    let long = [
        (
            "resource_exhausted",
            http("200 OK", "application/proto", &body),
        ),
        ("an HTTP 429 that is not Connect's", error(most + 1)),
    ];
    for op in Operation::ALL.iter().copied() {
        for (why, answer) in &long {
            let client = stub.answering(answer.clone());
            let e = ask_one(&client, &call(KEY), op).await.unwrap_err();
            let ended = (e.kind, e.hint.as_deref());
            assert_eq!(ended, unanswered(op), "{op:?} {why}");
            assert_eq!(e.message, format!("the engine did not answer: {why}"));
        }
    }
    let client = stub.answering(error(most));
    let e = client.emit(&call(KEY), item("a")).await.unwrap_err();
    let refused = OpError {
        kind: ErrorKind::ResourceExhausted,
        message: "the call was refused: resource_exhausted".into(),
        hint: None,
    };
    assert_eq!(e, refused);
}

/// SRV-4, rule 4: a code of the server's library, without the engine's own error, under the HTTP
/// status a server of Connect's gives it.
#[tokio::test]
async fn a_code_of_the_library_is_the_kind_it_means() {
    let stub = Stub::new().await;
    let older = Some("the engine is older than this client");
    let invalid = Some((ErrorKind::Invalid, None));
    let full = Some((ErrorKind::ResourceExhausted, None));
    let internal = Some((ErrorKind::Internal, older));
    let codes = [
        ("invalid_argument", 400, invalid),
        ("out_of_range", 400, invalid),
        ("failed_precondition", 400, invalid),
        ("resource_exhausted", 429, full),
        ("unimplemented", 404, internal),
        ("unimplemented", 501, internal),
        // The engine's own always come with its error, so these are no word of its.
        ("unauthenticated", 401, None),
        ("permission_denied", 403, None),
        ("not_found", 404, None),
        ("unavailable", 503, None),
        ("internal", 500, None),
        ("unknown", 500, None),
        ("canceled", 499, None),
        ("deadline_exceeded", 504, None),
        ("already_exists", 409, None),
        ("aborted", 409, None),
        ("data_loss", 500, None),
    ];
    for op in Operation::ALL.iter().copied() {
        for (i, (code, own, kind)) in codes.into_iter().enumerate() {
            let kind_sent = [
                "application/json",
                "Application/JSON; charset=utf-8",
                "application/json ;charset=\"UTF-8\"",
            ][i % 3];
            let body = format!(r#"{{"code":"{code}","message":"{CREATED}"}}"#);
            let client = stub.answering(http(&format!("{own} x"), kind_sent, body.as_bytes()));
            let e = ask_one(&client, &call(KEY), op).await.unwrap_err();
            let ended = (e.kind, e.hint.as_deref());
            let expected = kind.unwrap_or_else(|| unanswered(op));
            assert_eq!(ended, expected, "{op:?} {code}");
            let began = match kind {
                Some(_) => "the call was refused",
                None => "the engine did not answer",
            };
            assert_eq!(e.message, format!("{began}: {code}"), "{op:?}");
        }
    }
    // Under another, a code is none of Connect's, whatever the operation.
    for op in [Operation::Emit, Operation::GetEvent] {
        for (code, ..) in codes.iter().filter(|(.., kind)| kind.is_some()) {
            for status in [400, 404, 429, 500, 501, 503] {
                if codes.iter().any(|(c, own, _)| (c, *own) == (code, status)) {
                    continue;
                }
                let body = format!(r#"{{"code":"{code}","message":"{CREATED}"}}"#);
                let client = stub.answering(json(&format!("{status} x"), &body));
                let e = ask_one(&client, &call(KEY), op).await.unwrap_err();
                let ended = (e.kind, e.hint.as_deref());
                assert_eq!(ended, unanswered(op), "{op:?} {code} {status}");
                let message =
                    format!("the engine did not answer: an HTTP {status} that is not Connect's");
                assert_eq!(e.message, message, "{op:?} {code}");
            }
        }
    }
}

/// SRV-4: from a door that has no such operation, as an older engine's has not.
#[tokio::test]
async fn an_older_engine_says_so() {
    use connectrpc::{ConnectRpcService, Router};
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = Client::new(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
    let none = axum::Router::new().fallback_service(ConnectRpcService::new(Router::new()));
    tokio::spawn(async move { axum::serve(listener, none).await });
    for op in Operation::ALL.iter().copied() {
        let e = ask_one(&client, &call(KEY), op).await.unwrap_err();
        let ended = (e.kind, e.hint.as_deref());
        let older = Some("the engine is older than this client");
        assert_eq!(ended, (ErrorKind::Internal, older), "{op:?}");
        assert_eq!(e.message, "the call was refused: unimplemented");
    }
}

/// SRV-3, rule 1: the engine's own error, whatever came of the answer around it: a code this client
/// does not know, and a status, a type or an encoding the engine did not give it.
#[tokio::test]
async fn the_engines_own_error_is_first() {
    let stub = Stub::new().await;
    // 'kind' "not_found", 'message' "m".
    let detail = r#"[{"type":"ironflowx.v1.ErrorInfo","value":"Cglub3RfZm91bmQSAW0"}]"#;
    let field =
        r#"[{"type":"ironflowx.v1.ErrorInfo","value":"Cglub3RfZm91bmQSAW0","x":"é\ud800"}]"#;
    let heads = [
        "404 x\r\ncontent-type: application/json",
        "503 x\r\ncontent-type: application/json",
        "503 x\r\ncontent-type: application/json; profile=rpc",
        "503 x\r\ncontent-type: text/html",
        "503 x",
        // Said to be compressed, and not.
        "404 x\r\ncontent-type: application/json\r\ncontent-encoding: gzip",
    ];
    for head in heads {
        for rest in [
            "",
            r#""code":"new","#,
            r#""code":"unimplemented","message":"x","#,
            r#""code":null,"message":null,"#,
            r#""message":"é","#,
        ] {
            let detail = if rest.is_empty() { field } else { detail };
            let body = format!(r#"{{{rest}"details":{detail}}}"#);
            let length = body.len();
            let answer = format!("HTTP/1.1 {head}\r\ncontent-length: {length}\r\n\r\n{body}");
            for op in Operation::ALL.iter().copied() {
                let client = stub.answering(answer.clone().into_bytes());
                let e = ask_one(&client, &call(KEY), op).await.unwrap_err();
                let own = OpError {
                    kind: ErrorKind::NotFound,
                    message: "m".into(),
                    hint: None,
                };
                assert_eq!(e, own, "{op:?} {rest} {head}");
            }
        }
    }
}

/// Answers one request with `head` and a copy of the request's body in the place of `wrap`'s `@`,
/// and gives what it answered.
async fn copying(head: &str, wrap: &'static str) -> (Client, JoinHandle<Vec<u8>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = Client::new(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
    let head = head.to_owned();
    let answered = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut read = Vec::new();
        let body = loop {
            if let Some(end) = read.windows(4).position(|w| w == b"\r\n\r\n") {
                let text = String::from_utf8_lossy(&read[..end]).to_lowercase();
                let length = text.split("\r\ncontent-length: ").nth(1);
                // None of a request that holds nothing.
                let length = length.map_or(0, |l| l.lines().next().unwrap().parse().unwrap());
                if read.len() >= end + 4 + length {
                    break read[end + 4..].to_vec();
                }
            }
            assert_ne!(stream.read_buf(&mut read).await.unwrap(), 0);
        };
        let (before, after) = wrap.split_once('@').unwrap();
        let body = [before.as_bytes(), &body, after.as_bytes()].concat();
        let length = body.len();
        let head = format!("HTTP/1.1 {head}\r\ncontent-length: {length}\r\n\r\n");
        let _ = stream.write_all(&[head.as_bytes(), &body].concat()).await;
        body
    });
    (client, answered)
}

/// How an emit of this name, and this idempotency key, is sent.
fn encoded(request: &EmitRequest) -> Vec<u8> {
    let field = |tag: u8, text: &str| {
        let length = u8::try_from(text.len()).expect("a length of one byte");
        [&[tag, length], text.as_bytes()].concat()
    };
    let key = request.idempotency_key.as_ref();
    let key = key.map_or_else(Vec::new, |key| field(0x2a, key));
    [field(0x0a, &request.name), key].concat()
}

/// SRV-4: an event's name or its idempotency key can make a request, copied as it is into an
/// answer, part of an engine's error there. No such answer is one: the event may be stored.
#[tokio::test]
async fn a_request_copied_into_an_answer_is_no_engines_error() {
    // ErrorInfo of 'kind' "unavailable", which may be sent again at once, and "invalid".
    let unavailable = r#"{"type":"ironflowx.v1.ErrorInfo","value":"Cgt1bmF2YWlsYWJsZQ"}"#;
    let invalid = r#"{"type":"ironflowx.v1.ErrorInfo","value":"CgdpbnZhbGlk"}"#;
    let filled = |text: String, length: usize, end: &str| {
        let fill = "1".repeat(length - text.len() - end.len());
        format!("{text}{fill}{end}")
    };
    let unkeyed = |name: String| EmitRequest {
        name,
        data: None,
        metadata: None,
        version: None,
        idempotency_key: None,
    };
    // A request begins with a newline and its name's length: 123 is a brace, 91 a bracket, and 34 a
    // quote, so that the name is an object, a list, or the end of a text.
    let object = unkeyed(filled(
        format!(r#""code":"unavailable","details":[{unavailable}],"x":""#),
        123,
        "\"}",
    ));
    let closing = unkeyed(filled(
        format!(r#""a":1}},"details":[{unavailable}],"x":1"#),
        123,
        "",
    ));
    let fields = |info: &str| info[1..info.len() - 1].to_owned();
    let element = unkeyed(filled(
        format!(r#"{},"x":""#, fields(unavailable)),
        123,
        "\"}",
    ));
    let list = unkeyed(filled(
        format!(r#"{{{},"x":""#, fields(unavailable)),
        91,
        "\"}]",
    ));
    let text = EmitRequest {
        idempotency_key: Some(format!(r#"","details":[{invalid}]"#)),
        ..unkeyed("a".repeat(34))
    };
    let mut rows = Vec::new();
    for kind in [
        "\r\ncontent-type: application/json",
        "\r\ncontent-type: application/json; profile=rpc",
        "\r\ncontent-type: text/html",
        "\r\ncontent-type: application/proto",
        "",
    ] {
        rows.push((format!("502 x{kind}"), "@", &object));
    }
    for kind in ["application/json", "text/html"] {
        let head = format!("502 x\r\ncontent-type: {kind}");
        for wrap in [
            r#"{"error":"bad gateway","request":@}"#,
            r#"{"code":"x","request":@}"#,
            r#"{"message":@}"#,
            r#"{"code":@}"#,
        ] {
            rows.push((head.clone(), wrap, &closing));
        }
        rows.push((head.clone(), r#"{"message":@}"#, &text));
        rows.push((head.clone(), r#"{"code":@}"#, &text));
        rows.push((head.clone(), r#"{"error":"bad","message":@}"#, &text));
        rows.push((
            head.clone(),
            r#"{"code":"x","message":"y","details":[@]}"#,
            &element,
        ));
        rows.push((head, r#"{"message":"y","details":@}"#, &list));
    }
    let json = "503 x\r\ncontent-type: application/json".to_owned();
    rows.push((json, r#"{"code":"unavailable","message":@}"#, &text));
    rows.push(("503 x\r\ncontent-type: text/html".to_owned(), "@", &object));
    let (mut got, mut expected) = (Vec::new(), Vec::new());
    for (head, wrap, request) in rows {
        let (client, answered) = copying(&head, wrap).await;
        let e = client.emit(&call(KEY), request.clone()).await.unwrap_err();
        let (before, after) = wrap.split_once('@').unwrap();
        let copied = [before.as_bytes(), &encoded(request), after.as_bytes()].concat();
        assert_eq!(answered.await.unwrap(), copied, "{wrap}");
        let hint = match request.idempotency_key {
            Some(_) => "retry with the same idempotency key",
            None => "check first what landed: a retry may store a second event",
        };
        let unknown = OpError {
            kind: ErrorKind::Unknown,
            message: format!(
                "the engine did not answer: an HTTP {} that is not Connect's",
                &head[..3]
            ),
            hint: Some(hint.into()),
        };
        got.push((head.clone(), wrap, e));
        expected.push((head, wrap, unknown));
    }
    assert_eq!(got, expected);
}

/// Rule 4's known limit, with nothing of the request in the answer to tell it by: a `create_key`
/// that asks for nothing is empty, so a copy of it is.
#[tokio::test]
async fn an_answer_as_a_connect_server_sends_is_taken_for_one() {
    let wrap = r#"{"code":"invalid_argument","message":"@"}"#;
    let (client, _) = copying("400 x\r\ncontent-type: application/json", wrap).await;
    let empty = CreateKeyRequest::default();
    let e = client.create_key(&call(KEY), empty).await.unwrap_err();
    let refused = OpError {
        kind: ErrorKind::Invalid,
        message: "the call was refused: invalid_argument".into(),
        hint: None,
    };
    assert_eq!(e, refused);
}

/// The hint of an `Unknown` fits the request: a retry of an emit stores one event only if it has an
/// idempotency key.
#[tokio::test]
async fn an_unknown_emits_hint_fits_its_key() {
    let stub = Stub::new().await;
    let unkeyed = EmitRequest {
        idempotency_key: None,
        ..item("a")
    };
    let check = "check first what landed: a retry may store a second event";
    for (request, hint) in [
        (item("a"), unanswered(Operation::Emit).1.unwrap()),
        (unkeyed.clone(), check),
    ] {
        let client = stub.answering(vec![]);
        let e = client.emit(&call(KEY), request).await.unwrap_err();
        assert_eq!(
            (e.kind, e.hint.as_deref()),
            (ErrorKind::Unknown, Some(hint))
        );
    }
    // One for any batch: it says which may be sent again.
    for items in [
        vec![item("a")],
        vec![item("a"), unkeyed.clone()],
        vec![unkeyed],
    ] {
        let client = stub.answering(vec![]);
        let batch = EmitBatchRequest { items };
        let e = client.emit_batch(&call(KEY), batch).await.unwrap_err();
        let ended = (e.kind, e.hint.as_deref());
        assert_eq!(ended, unanswered(Operation::EmitBatch));
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
        // Kept open, or the later calls would not connect.
        (
            (stream, listener),
            String::from_utf8_lossy(&head).to_lowercase(),
        )
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
    let (_open, head) = asked.expect("a call").unwrap();
    assert!(head.contains("\r\nconnect-timeout-ms: 250\r\n"), "{head}");
}
