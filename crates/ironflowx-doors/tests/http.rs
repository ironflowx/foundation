//! The HTTP door in front of a recording `Operations`.

mod common;

use std::net::SocketAddr;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{KEY, Ready, Recording, serve, serving, whoami};
use connectrpc::client::{CallOptions, ClientConfig, HttpClient};
use connectrpc::{ErrorCode, Protocol};
use ironflowx_core::ErrorKind;
use ironflowx_doors::{Slot, WaitingOn};
use ironflowx_proto::wire::{self, IdentityServiceClient, WhoamiRequest, WhoamiResponse};
use ironflowx_proto::{OpError, Operation, Whoami};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;

const JSON: (&str, &[u8]) = ("content-type", b"application/json");

fn bearer() -> CallOptions {
    CallOptions::default().with_header("authorization", format!("Bearer {KEY}"))
}

const PROTOCOLS: [&str; 4] = ["Connect JSON", "Connect binary", "gRPC", "gRPC-Web"];

fn client_of(protocol: &str, addr: SocketAddr) -> IdentityServiceClient<HttpClient> {
    let config = ClientConfig::new(format!("http://{addr}").parse().unwrap());
    let (http, config) = match protocol {
        "Connect JSON" => (HttpClient::plaintext(), config.json()),
        "Connect binary" => (HttpClient::plaintext(), config),
        "gRPC" => (
            HttpClient::plaintext_http2_only(),
            config.with_protocol(Protocol::Grpc),
        ),
        "gRPC-Web" => (
            HttpClient::plaintext(),
            config.with_protocol(Protocol::GrpcWeb),
        ),
        _ => unreachable!(),
    };
    IdentityServiceClient::new(http, config)
}

/// Sends one HTTP/1.0 request, whose answer is not chunked, and reads the answer to its end: its
/// head and its body.
async fn send(
    addr: SocketAddr,
    line: &str,
    headers: &[(&str, &[u8])],
    body: &[u8],
) -> (String, String) {
    let mut head = format!("{line} HTTP/1.0\r\n").into_bytes();
    for (name, value) in headers {
        head.extend([name.as_bytes(), b": ", value, b"\r\n"].concat());
    }
    head.extend(format!("content-length: {}\r\n\r\n", body.len()).into_bytes());
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream.write_all(&head).await.unwrap();
    // The door may refuse a body before all of it is written.
    let _ = stream.write_all(body).await;
    let mut answer = Vec::new();
    let _ = stream.read_to_end(&mut answer).await;
    let answer = String::from_utf8(answer).unwrap();
    let (head, body) = answer.split_once("\r\n\r\n").expect("an answer");
    (head.to_owned(), body.to_owned())
}

const WHOAMI: &str = "POST /ironflowx.v1.IdentityService/Whoami";

/// SRV-2: what is served, at the name its operation has.
#[tokio::test]
async fn an_operation_is_served_at_its_rpc() {
    let (addr, operations) = serving(Ok(whoami())).await;
    for op in Operation::ALL {
        let line = format!("POST /{}", op.rpc());
        let (head, _) = send(addr, &line, &[JSON], b"{}").await;
        let served = *op == Operation::Whoami;
        let status = if served { "200" } else { "404" };
        assert!(
            head.starts_with(&format!("HTTP/1.0 {status} ")),
            "{line}: {head}"
        );
        assert_eq!(operations.take().len(), usize::from(served), "{line}");
    }
}

/// SRV-1: one call in, unchanged, and its answer out, on every protocol.
#[tokio::test(flavor = "multi_thread")]
async fn a_call_and_its_answer_cross_every_protocol() {
    let (addr, operations) = serving(Ok(whoami())).await;
    for protocol in PROTOCOLS {
        let client = client_of(protocol, addr);
        let answer = client
            .whoami_with_options(WhoamiRequest::default(), bearer())
            .await
            .unwrap();
        let answer = Whoami::try_from(answer.into_owned()).unwrap();
        assert_eq!(answer, whoami(), "{protocol}");
        assert_eq!(operations.take(), [Some(KEY.to_owned())], "{protocol}");
    }
}

const ANSWER: &str = r#"{"key_id":"0190b5f0-0000-7000-8000-000000000001","scopes":["read","admin"],"environment":"default/main","expires_at":"2027-01-15T08:00:00.123Z"}"#;

/// D78: the proto's own names, and the time in RFC 3339, whole milliseconds.
#[tokio::test]
async fn the_json_names_are_the_protos() {
    let (addr, _) = serving(Ok(whoami())).await;
    let (head, body) = send(addr, WHOAMI, &[JSON], b"{}").await;
    assert!(head.starts_with("HTTP/1.0 200 "), "{head}");
    assert_eq!(body, ANSWER);
}

/// SRV-3: kind, message and hint, on every protocol, under the kind's Connect code.
#[tokio::test(flavor = "multi_thread")]
async fn every_error_crosses_every_protocol() {
    let (addr, operations) = serving(Ok(whoami())).await;
    for (kind, code) in [
        (ErrorKind::Invalid, ErrorCode::InvalidArgument),
        (ErrorKind::Unauthenticated, ErrorCode::Unauthenticated),
        (ErrorKind::PermissionDenied, ErrorCode::PermissionDenied),
        (ErrorKind::NotFound, ErrorCode::NotFound),
        (ErrorKind::Unknown, ErrorCode::Unknown),
        (ErrorKind::ResourceExhausted, ErrorCode::ResourceExhausted),
        (ErrorKind::Unavailable, ErrorCode::Unavailable),
        (ErrorKind::Internal, ErrorCode::Internal),
    ] {
        for (message, hint) in [
            ("missing scope: réad % \"x\"".to_owned(), None),
            ("m".repeat(4_096), None),
            (
                String::new(),
                Some("créez une clé\nwith scope read".to_owned()),
            ),
        ] {
            let sent = OpError {
                kind,
                message,
                hint,
            };
            *operations.answer.lock().unwrap() = Err(sent.clone());
            for protocol in PROTOCOLS {
                let client = client_of(protocol, addr);
                let e = client.whoami(WhoamiRequest::default()).await.err().unwrap();
                assert_eq!(e.code, code, "{protocol}");
                assert_eq!(e.message.as_ref(), Some(&sent.message), "{protocol}");
                assert_eq!(wire::op_error(&e), Some(sent.clone()), "{protocol}");
            }
        }
    }
}

type Headers = Vec<(&'static str, Vec<u8>)>;

/// The Wire table's "Key" row: what each request presents to the engine.
#[tokio::test]
async fn the_key_is_the_one_authorization_headers_and_nothing_elses() {
    let (addr, operations) = serving(Ok(whoami())).await;
    let key = KEY.as_bytes();
    let with = |before: &str| [before.as_bytes(), key].concat();
    let none: Option<&str> = None;
    let malformed = Some("malformed");
    let auth = "authorization";
    #[rustfmt::skip]
    let cases: Vec<(Headers, Option<&str>)> = vec![
        (vec![], none),
        (vec![(auth, with("Bearer "))], Some(KEY)),
        (vec![(auth, with("bearer "))], Some(KEY)),
        (vec![(auth, with("BEARER   "))], Some(KEY)),
        (vec![("AUTHORIZATION", with("Bearer "))], Some(KEY)),
        (vec![(auth, b"Bearer".to_vec())], none),
        (vec![(auth, b"Bearer ".to_vec())], none),
        (vec![(auth, b"Bearer x".to_vec())], Some("x")),
        // No Bearer key.
        (vec![(auth, vec![])], malformed),
        (vec![(auth, with(""))], malformed),
        (vec![(auth, with("Basic "))], malformed),
        (vec![(auth, with("Basic Bearer "))], malformed),
        (vec![(auth, with("Bearer\t"))], malformed),
        (vec![(auth, with("Bearer:"))], malformed),
        (vec![(auth, with("Bearer"))], malformed),
        (vec![(auth, [b"Bearer ".as_slice(), key, b"\xff"].concat())], malformed),
        (vec![(auth, [b"Bearer ".as_slice(), key, "é".as_bytes()].concat())], malformed),
        (vec![(auth, with("Bearer ")), (auth, with("Bearer "))], malformed),
        (vec![(auth, with("Bearer ")), (auth, b"Basic x".to_vec())], malformed),
        (vec![(auth, vec![]), (auth, with("Bearer "))], malformed),
        // Nothing else is read.
        (vec![("x-test-credential", with(""))], none),
        (vec![("proxy-authorization", with("Bearer "))], none),
        (vec![("trailer-authorization", with("Bearer "))], none),
        (vec![("cookie", with("authorization=Bearer "))], none),
    ];
    for (headers, presented) in cases {
        let mut sent = vec![JSON];
        sent.extend(
            headers
                .iter()
                .map(|(name, value)| (*name, value.as_slice())),
        );
        let (head, _) = send(addr, WHOAMI, &sent, b"{}").await;
        assert!(head.starts_with("HTTP/1.0 200 "), "{head}");
        let shown = headers
            .iter()
            .map(|(name, value)| format!("{name}: {}", value.escape_ascii()))
            .collect::<Vec<_>>();
        let presented = presented.map(str::to_owned);
        assert_eq!(operations.take(), [presented], "{shown:?}");
    }
}

#[tokio::test]
async fn a_key_in_the_url_is_not_read() {
    let (addr, operations) = serving(Ok(whoami())).await;
    for line in [
        format!("{WHOAMI}?token={KEY}&authorization=Bearer%20{KEY}"),
        // A read, so Connect may call it with GET.
        format!(
            "GET /ironflowx.v1.IdentityService/Whoami?connect=v1&encoding=json&message=%7B%7D&token={KEY}"
        ),
    ] {
        let headers: &[_] = if line.starts_with("GET") {
            &[]
        } else {
            &[JSON]
        };
        let (head, _) = send(addr, &line, headers, b"{}").await;
        assert!(head.starts_with("HTTP/1.0 200 "), "{line}: {head}");
        assert_eq!(operations.take(), [None], "{line}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_slot_answers_for_its_engine_until_it_is_open() {
    let slot = Arc::new(Slot::starting());
    let addr = serve(slot.clone()).await;
    let starting = OpError {
        kind: ErrorKind::Unavailable,
        message: "the engine is unavailable".into(),
        hint: Some("the engine is starting".into()),
    };
    for protocol in PROTOCOLS {
        let client = client_of(protocol, addr);
        let e = client.whoami(WhoamiRequest::default()).await.err().unwrap();
        assert_eq!(wire::op_error(&e), Some(starting.clone()), "{protocol}");
    }
    let state = Arc::new(Mutex::new(Ok(())));
    let ready = async |expected: &str, body: &str| {
        for (path, status, body) in [("/health", "200", ""), ("/ready", expected, body)] {
            let (head, answered) = send(addr, &format!("GET {path}"), &[], b"").await;
            assert!(
                head.starts_with(&format!("HTTP/1.0 {status} ")),
                "{path}: {head}"
            );
            assert_eq!(answered, body, "{path}");
        }
    };
    ready("503", r#"{"ready":false,"waiting_on":"open"}"#).await;
    slot.open(Recording::answering(Ok(whoami())), Ready(state.clone()));
    ready("200", r#"{"ready":true}"#).await;
    for (waiting_on, body) in [
        (
            WaitingOn::ControlPlane,
            r#"{"ready":false,"waiting_on":"control_plane"}"#,
        ),
        (
            WaitingOn::Ledger,
            r#"{"ready":false,"waiting_on":"ledger"}"#,
        ),
    ] {
        *state.lock().unwrap() = Err(waiting_on);
        ready("503", body).await;
    }
    *state.lock().unwrap() = Ok(());
    ready("200", r#"{"ready":true}"#).await;
    for protocol in PROTOCOLS {
        let client = client_of(protocol, addr);
        let answer = client.whoami(WhoamiRequest::default()).await.unwrap();
        let answer: WhoamiResponse = answer.into_owned();
        assert_eq!(Whoami::try_from(answer), Ok(whoami()), "{protocol}");
    }
}

#[tokio::test]
async fn a_request_body_holds_at_most_16_mib() {
    let (addr, operations) = serving(Ok(whoami())).await;
    // A field the message does not have is ignored.
    let body = |size: usize| format!(r#"{{"x":"{}"}}"#, "a".repeat(size - 8)).into_bytes();
    let (head, _) = send(addr, WHOAMI, &[JSON], &body(16 * 1024 * 1024)).await;
    assert!(head.starts_with("HTTP/1.0 200 "), "{head}");
    assert_eq!(operations.take(), [None]);
    let (head, refused) = send(addr, WHOAMI, &[JSON], &body(16 * 1024 * 1024 + 1)).await;
    assert!(head.starts_with("HTTP/1.0 429 "), "{head}");
    assert!(
        refused.contains(r#""code":"resource_exhausted""#),
        "{refused}"
    );
    assert_eq!(operations.take(), []);
}

#[tokio::test]
async fn nothing_is_compressed() {
    let (addr, operations) = serving(Ok(whoami())).await;
    for encoding in [b"gzip".as_slice(), b"zstd"] {
        let headers = [JSON, ("content-encoding", encoding)];
        let (head, _) = send(addr, WHOAMI, &headers, b"{}").await;
        assert!(head.starts_with("HTTP/1.0 501 "), "{head}");
        assert_eq!(operations.take(), []);
        let headers = [JSON, ("accept-encoding", encoding)];
        let (head, body) = send(addr, WHOAMI, &headers, b"{}").await;
        assert!(head.starts_with("HTTP/1.0 200 "), "{head}");
        assert!(!head.contains("content-encoding"), "{head}");
        assert_eq!(body, ANSWER);
        assert_eq!(operations.take(), [None]);
    }
}

#[tokio::test]
async fn a_request_that_does_not_decode_is_refused_before_its_key_is_read() {
    let (addr, operations) = serving(Ok(whoami())).await;
    let (head, refused) = send(addr, WHOAMI, &[JSON], b"{").await;
    assert!(head.starts_with("HTTP/1.0 400 "), "{head}");
    assert!(
        refused.contains(r#""code":"invalid_argument""#),
        "{refused}"
    );
    assert_eq!(operations.take(), []);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_callers_deadline_is_honored_on_every_protocol() {
    let (addr, operations) = serving(Ok(whoami())).await;
    operations.hangs.store(true, Relaxed);
    let headers = [JSON, ("connect-timeout-ms", b"50")];
    let (head, refused) = send(addr, WHOAMI, &headers, b"{}").await;
    assert!(head.starts_with("HTTP/1.0 504 "), "{head}");
    assert!(
        refused.contains(r#""code":"deadline_exceeded""#),
        "{refused}"
    );
    for protocol in PROTOCOLS {
        let client = client_of(protocol, addr);
        let options = CallOptions::default().with_timeout(Duration::from_millis(50));
        let answer = client.whoami_with_options(WhoamiRequest::default(), options);
        let e = answer.await.err().unwrap();
        assert_eq!(e.code, ErrorCode::DeadlineExceeded, "{protocol}");
    }
    assert_eq!(operations.take().len(), 1 + PROTOCOLS.len());
}

#[test]
#[should_panic(expected = "a slot opens once")]
fn a_slot_opens_once() {
    let slot = Slot::starting();
    for _ in 0..2 {
        let state = Arc::new(Mutex::new(Ok(())));
        slot.open(Recording::answering(Ok(whoami())), Ready(state));
    }
}
