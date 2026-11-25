//! The proto client, through the HTTP door of a recording `Operations`.

mod common;

use std::collections::BTreeSet;
use std::sync::atomic::Ordering::Relaxed;
use std::time::Duration;

use common::{KEY, serving, whoami};
use ironflowx_core::{Address, ErrorKind, Secret};
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

/// A read without the engine's own error: no answer, or one that is not its own.
#[tokio::test]
async fn a_read_no_engine_answers_is_unavailable() {
    let closed = {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        format!("http://{}", listener.local_addr().unwrap())
    };
    let http = |status: &str, kind: &str, body: &str| {
        let len = body.len();
        format!("HTTP/1.1 {status}\r\ncontent-type: {kind}\r\ncontent-length: {len}\r\n\r\n{body}")
            .into_bytes()
    };
    for client in [
        Client::new(&closed).unwrap(),
        stub(vec![]).await,
        stub(http("401 Unauthorized", "text/html", "<html>")).await,
        stub(http("200 OK", "application/proto", "\u{7f}")).await,
        // A WhoamiResponse whose key ID is "x".
        stub(http("200 OK", "application/proto", "\n\x01x")).await,
        // ErrorInfo of a kind this client does not know: 'kind' "new".
        stub(http(
            "400 Bad Request",
            "application/json",
            r#"{"code":"invalid_argument","details":[{"type":"ironflowx.v1.ErrorInfo","value":"CgNuZXc"}]}"#,
        ))
        .await,
    ] {
        let e = ask(&client, &call(KEY)).await.unwrap_err();
        assert_eq!((e.kind, e.hint), (ErrorKind::Unavailable, None), "{}", e.message);
        assert!(!e.message.contains(KEY), "{}", e.message);
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
        let ended = tokio::time::timeout(late, ask(&client, &call(KEY))).await;
        let e = ended.expect("in time").unwrap_err();
        assert_eq!((e.kind, e.hint), (ErrorKind::Unavailable, None));
    }
    let (_stream, head) = asked.await.unwrap();
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
    let items = vec![emit.clone()];
    let id = || "x".to_owned();
    let page = ListEventsRequest {
        limit: None,
        cursor: None,
    };
    let refused = [
        client.emit(&cx, emit).await.err(),
        client
            .emit_batch(&cx, EmitBatchRequest { items })
            .await
            .err(),
        client
            .get_event(&cx, GetEventRequest { event_id: id() })
            .await
            .err(),
        client.list_events(&cx, page).await.err(),
        client
            .list_environments(&cx, ListEnvironmentsRequest {})
            .await
            .err(),
        client
            .create_key(&cx, CreateKeyRequest::default())
            .await
            .err(),
        client
            .list_keys(&cx, ListKeysRequest::default())
            .await
            .err(),
        client
            .revoke_key(&cx, RevokeKeyRequest { key_id: id() })
            .await
            .err(),
    ];
    let expected: Vec<_> = Operation::ALL
        .iter()
        .filter(|op| **op != Operation::Whoami)
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
