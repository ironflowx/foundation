//! The proto client, through the HTTP door of a recording `Operations`.

mod common;

use std::collections::BTreeSet;
use std::num::NonZeroU32;
use std::sync::atomic::Ordering::Relaxed;
use std::time::Duration;

use common::{CREATED, KEY, created, environments, key_page, serving, whoami};
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
    for kind in [
        ErrorKind::Invalid,
        ErrorKind::Unauthenticated,
        ErrorKind::PermissionDenied,
        ErrorKind::NotFound,
        ErrorKind::Unknown,
        ErrorKind::ResourceExhausted,
        ErrorKind::Unavailable,
        ErrorKind::Internal,
    ] {
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

/// The operations the client carries.
const CARRIED: [Operation; 5] = [
    Operation::Whoami,
    Operation::ListEnvironments,
    Operation::CreateKey,
    Operation::ListKeys,
    Operation::RevokeKey,
];

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
        op => unreachable!("{op:?} is not carried"),
    }
}

/// SRV-1, SRV-3: each request arrives as it was asked, once, with its call's key; the answer, or
/// the error, comes back unchanged.
#[tokio::test]
async fn each_operation_crosses_to_the_engine_and_back() {
    let (addr, operations) = serving(Ok(whoami())).await;
    let client = Client::new(&format!("http://{addr}")).unwrap();
    let answers = [
        format!("{:?}", whoami()),
        format!("{:?}", environments()),
        format!("{CREATED} {:?}", created().info),
        format!("{:?}", key_page()),
        "()".to_owned(),
    ];
    for (op, answer) in CARRIED.into_iter().zip(answers) {
        assert_eq!(ask_one(&client, &call(KEY), op).await, Ok(answer), "{op:?}");
    }
    assert_eq!(operations.take(), vec![Some(KEY.to_owned()); 5]);
    let asked = [
        "WhoamiRequest",
        "ListEnvironmentsRequest",
        r#"CreateKeyRequest { scopes: {Write}, expires_at: Some(Timestamp { millis: 1800000000001 }), label: Some("clé") }"#,
        r#"ListKeysRequest { limit: Some(7), cursor: Some("c") }"#,
        r#"RevokeKeyRequest { key_id: "no ID" }"#,
    ];
    assert_eq!(operations.take_requests(), asked);

    let e = OpError {
        kind: ErrorKind::NotFound,
        message: "m".into(),
        hint: Some("h".into()),
    };
    *operations.answer.lock().unwrap() = Err(e.clone());
    for op in CARRIED {
        let answer = ask_one(&client, &call(KEY), op).await;
        assert_eq!(answer, Err(e.clone()), "{op:?}");
    }
    assert_eq!(operations.take().len(), 5);
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
        assert_eq!((e.kind, e.hint), (ErrorKind::Invalid, None), "{outside}");
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
            Some("retry: revoking a revoked key succeeds"),
        ),
        _ => (ErrorKind::Unavailable, None),
    }
}

/// No answer, or one that is not the engine's own: a read is `Unavailable`, a state change
/// `Unknown`, and no message holds a key (TEN-3).
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
    let cut = [&key, b"\x12\x7fx".as_slice()].concat();
    // A WhoamiResponse or a CreateKeyResponse whose first field is "x"; as a page, one that holds
    // a message that does not decode.
    let short = b"\n\x01x";
    let answers = [
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
    let ignored = [Some(&answers[5]), Some(&answers[6])];
    for op in CARRIED {
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
            let e = got.unwrap_err();
            let ended = (e.kind, e.hint.as_deref());
            assert_eq!(ended, unanswered(op), "{op:?} {shown:?}");
            for key in [KEY, CREATED] {
                assert!(!e.message.contains(&key[41..]), "{}", e.message);
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
        for op in CARRIED {
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

/// The Errors table's `Internal` row.
#[tokio::test]
async fn an_operation_the_client_does_not_carry_is_internal() {
    let client = Client::new("http://127.0.0.1:9").unwrap();
    let cx = Call::anonymous();
    let emit = EmitRequest {
        name: "a".into(),
        data: None,
        metadata: None,
        version: None,
        idempotency_key: None,
    };
    let batch = EmitBatchRequest {
        items: vec![emit.clone()],
    };
    let get = GetEventRequest {
        event_id: "x".into(),
    };
    let page = ListEventsRequest {
        limit: None,
        cursor: None,
    };
    let refused = [
        client.emit(&cx, emit).await.err(),
        client.emit_batch(&cx, batch).await.err(),
        client.get_event(&cx, get).await.err(),
        client.list_events(&cx, page).await.err(),
    ];
    let expected: Vec<_> = Operation::ALL
        .iter()
        .filter(|op| !CARRIED.contains(op))
        .map(|op| {
            Some(OpError {
                kind: ErrorKind::Internal,
                message: format!("the client does not carry {}", op.rpc()),
                hint: None,
            })
        })
        .collect();
    assert_eq!(refused.to_vec(), expected);
}
