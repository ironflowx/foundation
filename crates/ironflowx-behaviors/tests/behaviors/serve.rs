//! `serve.feature` on the `ironflowx` binary, started by each scenario.

#[allow(dead_code)]
#[path = "../../../ironflowx-operations/tests/leak/mod.rs"]
mod leak;

use std::fmt;
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::TcpStream;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use connectrpc::client::{
    BoxFuture, CallOptions, ClientBody, ClientConfig, ClientTransport, HttpClient, full_body,
};
use connectrpc::{ConnectError, ErrorCode, Protocol};
use cucumber::writer::Stats as _;
use cucumber::{World, given, then, when};
use ironflowx_core::{ErrorKind, KeyOrigin, Scope, Secret};
use ironflowx_proto::wire::{
    CreateKeyRequest, CreateKeyResponse, EmitBatchRequest, EmitRequest, Event, EventServiceClient,
    GetEventRequest, IdentityServiceClient, KeyServiceClient, WhoamiRequest, WhoamiResponse,
    op_error,
};
use ironflowx_proto::{
    Call, Client, EmitBatchResponse, EmitResponse, ListEventsRequest, ListKeysRequest, OpError,
    Operation, Operations as _,
};
use serde_json::Value;
use tempfile::TempDir;

/// The scenarios whose work has landed: every one must run and pass.
const REGISTERED: &[&str] = &[
    "HOST-001", "HOST-006", "HOST-016", "HOST-021", "HOST-022", "HOST-023", "HOST-024", "HOST-025",
    "HOST-026", "HOST-027", "HOST-029",
];

/// How many scenarios run, an outline once per example, so no example goes unnoticed.
pub const RUNS: usize = 28;

const LATE: Duration = Duration::from_secs(30);

static BINARY: OnceLock<PathBuf> = OnceLock::new();

/// Builds the binary, as an operator would: Cargo gives a test a binary's path only within its own
/// package.
pub fn build() {
    let out = Command::new(env!("CARGO"))
        .args(["build", "-p", "ironflowx-bin", "--locked"])
        .arg("--message-format=json")
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .stderr(Stdio::inherit())
        .output()
        .unwrap();
    assert!(out.status.success(), "ironflowx-bin builds");
    let binary = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|m| m["target"]["name"] == "ironflowx" && m["target"]["kind"][0] == "bin")
        .find_map(|m| m["executable"].as_str().map(PathBuf::from))
        .expect("cargo names the binary");
    BINARY.set(binary).expect("built once");
}

/// The binary, with no variable of ours set.
pub fn command() -> Command {
    let mut command = Command::new(BINARY.get().expect("the binary is built"));
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("IRONFLOWX_") {
            command.env_remove(name);
        }
    }
    command
}

/// A started `ironflowx serve`, killed when dropped, and what it has logged.
pub struct Served {
    child: Child,
    log: Arc<Mutex<Vec<String>>>,
    /// Ends once the whole log is in `log`.
    reader: Option<JoinHandle<()>>,
}

impl Drop for Served {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Served {
    /// Stops it; all it logged.
    fn stop(mut self) -> String {
        self.child.kill().unwrap();
        self.reader.take().unwrap().join().unwrap();
        self.log()
    }

    pub fn start(data_dir: &Path, listen: &str) -> Self {
        let mut child = command()
            .args(["serve", "--listen", listen, "--data-dir"])
            .arg(data_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stderr = BufReader::new(child.stderr.take().unwrap());
        let log = Arc::<Mutex<Vec<String>>>::default();
        let kept = log.clone();
        let reader = std::thread::spawn(move || {
            for line in stderr.lines().map_while(Result::ok) {
                kept.lock().unwrap().push(line);
            }
        });
        Self {
            child,
            log,
            reader: Some(reader),
        }
    }

    fn log(&self) -> String {
        self.log.lock().unwrap().join("\n")
    }

    /// The URLs of the `listening` lines so far.
    fn listening(&self) -> Vec<String> {
        let log = self.log.lock().unwrap();
        log.iter()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter(|line| line["fields"]["message"] == "listening")
            .map(|line| line["fields"]["url"].as_str().expect("a URL").to_owned())
            .collect()
    }

    pub fn url(&self) -> String {
        until("listening", || self.listening().into_iter().next())
    }

    /// Waits until `/ready` answers 200.
    pub fn ready(&self) {
        let url = self.url();
        until("ready", || (get(&url, "/ready") == 200).then_some(()));
    }
}

fn until<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let started = Instant::now();
    loop {
        if let Some(value) = f() {
            return value;
        }
        assert!(started.elapsed() < LATE, "not {what} in time");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The status `GET <path>` answers, with no key.
fn get(url: &str, path: &str) -> u16 {
    let addr = url.strip_prefix("http://").expect("an http URL");
    let mut stream = TcpStream::connect(addr).unwrap();
    write!(stream, "GET {path} HTTP/1.0\r\n\r\n").unwrap();
    let mut answer = String::new();
    stream.read_to_string(&mut answer).unwrap();
    let status = answer.split(' ').nth(1).expect("a status");
    status.parse().unwrap()
}

/// What the RPC answers, as the JSON it sends.
fn post_json(url: &str, rpc: &str, key: &str, body: &str) -> Value {
    let addr = url.strip_prefix("http://").expect("an http URL");
    let mut stream = TcpStream::connect(addr).unwrap();
    let length = body.len();
    write!(
        stream,
        "POST /{rpc} HTTP/1.0\r\nauthorization: Bearer {key}\r\n\
         content-type: application/json\r\ncontent-length: {length}\r\n\r\n{body}"
    )
    .unwrap();
    let mut answer = String::new();
    stream.read_to_string(&mut answer).unwrap();
    let (head, body) = answer.split_once("\r\n\r\n").expect("an answer");
    assert!(head.starts_with("HTTP/1.0 200 "), "{head}");
    serde_json::from_str(body).unwrap()
}

#[derive(World)]
#[world(init = Self::new)]
pub struct Serve {
    served: Option<Served>,
    dir: TempDir,
    /// The admin key's text.
    key: Option<String>,
    answer: Option<Result<WhoamiResponse, ConnectError>>,
    created: Option<Result<CreateKeyResponse, ConnectError>>,
    /// The client's key, where it is not the admin key, and the observer's.
    client_key: Option<String>,
    observer_key: Option<String>,
    emitted: Option<Result<EmitResponse, ConnectError>>,
    batch: Option<Result<EmitBatchResponse, ConnectError>>,
    event: Option<Event>,
}

impl fmt::Debug for Serve {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Serve")
            .field("log", &self.served.as_ref().map(Served::log))
            .field("answer", &self.answer)
            .field("created", &self.created)
            .field("emitted", &self.emitted)
            .field("batch", &self.batch)
            .finish_non_exhaustive()
    }
}

/// Sends every request with `query` in its URL.
#[derive(Clone)]
struct WithQuery(HttpClient, String);

impl ClientTransport for WithQuery {
    type ResponseBody = <HttpClient as ClientTransport>::ResponseBody;
    type Error = <HttpClient as ClientTransport>::Error;

    fn send(
        &self,
        mut request: http::Request<ClientBody>,
    ) -> BoxFuture<'static, Result<http::Response<Self::ResponseBody>, Self::Error>> {
        let uri = format!("{}?{}", request.uri(), self.1);
        *request.uri_mut() = uri.parse().unwrap();
        self.0.send(request)
    }
}

/// Sends every request with this body in place of its own.
#[derive(Clone)]
struct WithBody(HttpClient, Vec<u8>);

impl ClientTransport for WithBody {
    type ResponseBody = <HttpClient as ClientTransport>::ResponseBody;
    type Error = <HttpClient as ClientTransport>::Error;

    fn send(
        &self,
        request: http::Request<ClientBody>,
    ) -> BoxFuture<'static, Result<http::Response<Self::ResponseBody>, Self::Error>> {
        let body = full_body(self.1.clone().into());
        self.0.send(request.map(|_| body))
    }
}

/// Calls that present `key`.
fn bearer(key: &str) -> CallOptions {
    CallOptions::default().with_header("authorization", format!("Bearer {key}"))
}

impl Serve {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        // Not a setgid bit, which a directory made in it takes on Linux, as this one may have.
        std::fs::set_permissions(dir.path(), PermissionsExt::from_mode(0o700)).unwrap();
        Self {
            served: None,
            dir,
            key: None,
            answer: None,
            created: None,
            client_key: None,
            observer_key: None,
            emitted: None,
            batch: None,
            event: None,
        }
    }

    /// Not there until the host creates it.
    fn data_dir(&self) -> PathBuf {
        self.dir.path().join("data")
    }

    fn serve(&mut self, listen: &str) {
        self.served = Some(Served::start(&self.data_dir(), listen));
    }

    fn served(&self) -> &Served {
        self.served.as_ref().expect("a served engine")
    }

    fn key(&self) -> &str {
        self.key.as_deref().expect("an admin key")
    }

    /// The key in `admin.key`.
    fn read_key(&mut self) {
        let text = std::fs::read_to_string(self.data_dir().join("admin.key")).unwrap();
        let key = text.strip_suffix('\n').expect("a newline").to_owned();
        assert_eq!(key.len(), 84, "one whole key");
        self.key = Some(key);
    }

    fn config(&self) -> ClientConfig {
        ClientConfig::new(self.served().url().parse().unwrap())
    }

    /// What a client of `protocol` is made of.
    fn transport(&self, protocol: &str) -> (HttpClient, ClientConfig) {
        match protocol {
            "Connect JSON" => (HttpClient::plaintext(), self.config().json()),
            "Connect binary" => (HttpClient::plaintext(), self.config().proto()),
            "gRPC" => (
                HttpClient::plaintext_http2_only(),
                self.config().with_protocol(Protocol::Grpc),
            ),
            "gRPC-Web" => (
                HttpClient::plaintext(),
                self.config().with_protocol(Protocol::GrpcWeb),
            ),
            _ => unreachable!(),
        }
    }

    fn events(&self, protocol: &str) -> EventServiceClient<HttpClient> {
        let (http, config) = self.transport(protocol);
        EventServiceClient::new(http, config)
    }

    fn client_key(&self) -> &str {
        self.client_key.as_deref().expect("a client")
    }

    /// Creates a key with the scopes `names` quotes, with the admin key; its text.
    async fn create(&self, names: &str) -> String {
        let client = Client::new(&self.served().url()).unwrap();
        let cx = Call::with_key(Secret::new(self.key().to_owned()));
        let scopes = names.split('"').skip(1).step_by(2);
        let request = ironflowx_proto::CreateKeyRequest {
            scopes: scopes.map(|s| s.parse().unwrap()).collect(),
            ..Default::default()
        };
        let created = client.create_key(&cx, request).await.unwrap();
        created.key.expose().to_owned()
    }

    /// Why the client's call was refused.
    fn refusal(&self) -> &ConnectError {
        let emitted = self.emitted.as_ref().and_then(|a| a.as_ref().err());
        let batch = self.batch.as_ref().and_then(|a| a.as_ref().err());
        let created = self.created.as_ref().and_then(|a| a.as_ref().err());
        emitted.or(batch).or(created).expect("a refused call")
    }

    async fn whoami<T>(&mut self, client: IdentityServiceClient<T>, options: CallOptions)
    where
        T: ClientTransport,
        <T::ResponseBody as connectrpc::http_body::Body>::Error: fmt::Display,
    {
        let request = WhoamiRequest::default();
        let answer = client.whoami_with_options(request, options).await;
        self.answer = Some(answer.map(connectrpc::UnaryResponse::into_owned));
    }

    fn answered(&self) -> &Result<WhoamiResponse, ConnectError> {
        self.answer.as_ref().expect("a call")
    }
}

#[given("a fresh data directory")]
fn fresh(w: &mut Serve) {
    assert!(!w.data_dir().exists());
}

/// Every start drops them.
#[given("no IRONFLOWX_ variable is set except those the scenario names")]
fn no_variables(_: &mut Serve) {}

#[when("the engine is served on the data directory")]
fn served(w: &mut Serve) {
    w.serve("127.0.0.1:0");
}

#[when(expr = "the engine is served on {string}")]
fn served_on(w: &mut Serve, listen: String) {
    w.serve(&listen);
}

#[given("the engine is served and an admin key is known")]
#[given("the engine is served and a client holds the bootstrap key")]
fn served_with_key(w: &mut Serve) {
    w.serve("127.0.0.1:0");
    w.served().ready();
    w.read_key();
}

#[when("the engine is served and becomes ready")]
fn starts_and_becomes_ready(w: &mut Serve) {
    served(w);
    becomes_ready(w);
}

#[then(expr = "GET {string} answers 200 with no key")]
fn public_endpoint(w: &mut Serve, path: String) {
    assert_eq!(get(&w.served().url(), &path), 200);
}

#[then(regex = r#"^"ironflowx describe" lists neither$"#)]
fn no_health_operations(_: &mut Serve) {
    let out = command().arg("describe").output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert!(out.stderr.is_empty());
    let answer: Value = serde_json::from_slice(&out.stdout).unwrap();
    for op in answer["operations"].as_array().unwrap() {
        for field in ["command", "rpc"] {
            let name = op[field].as_str().unwrap().to_ascii_lowercase();
            assert!(
                !name.contains("health") && !name.contains("ready"),
                "{name}"
            );
        }
    }
}

#[then("the engine becomes ready")]
fn becomes_ready(w: &mut Serve) {
    w.served().ready();
}

fn mode(path: &Path) -> String {
    let mode = std::fs::metadata(path).unwrap().permissions().mode();
    format!("{:04o}", mode & 0o7777)
}

#[then(expr = "the data directory has mode {string}")]
fn directory_mode(w: &mut Serve, expected: String) {
    assert_eq!(mode(&w.data_dir()), expected);
}

#[then(expr = "{string} exists in the data directory with mode {string}")]
fn file_mode(w: &mut Serve, name: String, expected: String) {
    assert_eq!(mode(&w.data_dir().join(name)), expected);
}

#[then(expr = "the log names the path of {string} and does not contain the key")]
fn log_names(w: &mut Serve, name: String) {
    w.read_key();
    let path = w.data_dir().join(name);
    // As a log line holds it: a JSON string, its quotes included.
    let path = serde_json::to_string(&path.to_string_lossy()).unwrap();
    until("logged", || w.served().log().contains(&path).then_some(()));
    let log = w.served().log();
    assert_eq!(leak::leak(&log, [w.key()]), None, "{log}");
}

#[then(regex = r#"^with the key in "admin.key", whoami names scopes "read", "write" and "admin"$"#)]
async fn key_scopes(w: &mut Serve) {
    w.read_key();
    let client = Client::new(&w.served().url()).unwrap();
    let cx = Call::with_key(Secret::new(w.key().to_owned()));
    let me = client.whoami(&cx, ironflowx_proto::WhoamiRequest {}).await;
    let me = me.unwrap();
    assert_eq!(me.key_id.to_string(), w.key()[4..40]);
    let all = [Scope::Read, Scope::Write, Scope::Admin];
    assert_eq!(me.scopes, all.into());
    assert_eq!(me.expires_at, None);
    // The log, once it is whole.
    let log = w.served.take().expect("a served engine").stop();
    assert_eq!(leak::leak(&log, [w.key()]), None, "{log}");
}

#[then(regex = r#"^the log has one "listening" line whose URL answers GET "/health" with 200$"#)]
fn one_listening_line(w: &mut Serve) {
    w.served().ready();
    let urls = w.served().listening();
    assert_eq!(urls.len(), 1, "{}", w.served().log());
    assert!(!urls[0].ends_with(":0"), "{urls:?}");
    assert_eq!(get(&urls[0], "/health"), 200);
}

#[when(regex = "^whoami is called over Connect with (.+)$")]
async fn over_connect(w: &mut Serve, credential: String) {
    let key = w.key().to_owned();
    let header = |name: &str, value: String| CallOptions::default().with_header(name, value);
    let auth = |value: String| header("authorization", value);
    let options = match credential.as_str() {
        r#""Authorization: Bearer " and the admin key"# => auth(format!("Bearer {key}")),
        r#""Authorization: bearer " and the admin key"# => auth(format!("bearer {key}")),
        r#""Authorization: Basic " and the admin key"# => auth(format!("Basic {key}")),
        r#""Authorization: " and the admin key, no scheme"# => auth(key),
        r#""Authorization: Bearer" and nothing else"# => auth("Bearer".to_owned()),
        "no header" => CallOptions::default(),
        r#"the admin key in a header "X-Test-Credential""# => header("x-test-credential", key),
        r#"the admin key in a query parameter "token""# => {
            let transport = WithQuery(HttpClient::plaintext(), format!("token={key}"));
            let client = IdentityServiceClient::new(transport, w.config().json());
            return w.whoami(client, CallOptions::default()).await;
        }
        _ => panic!("unknown credential: {credential}"),
    };
    let client = IdentityServiceClient::new(HttpClient::plaintext(), w.config().json());
    w.whoami(client, options).await;
}

#[when(
    regex = "^whoami is called over (Connect JSON|Connect binary|gRPC|gRPC-Web) with the admin key$"
)]
async fn over_protocol(w: &mut Serve, protocol: String) {
    let (http, config) = w.transport(&protocol);
    let options = bearer(w.key());
    w.whoami(IdentityServiceClient::new(http, config), options)
        .await;
}

#[then("the answer names the admin key's ID")]
#[then("the call succeeds and names the admin key's ID")]
fn names_the_key(w: &mut Serve) {
    let answer = w.answered().as_ref().expect("the call succeeds");
    assert_eq!(answer.key_id, w.key()[4..40]);
}

#[then(expr = "the call is refused as unauthenticated with {string}")]
fn refused(w: &mut Serve, message: String) {
    let e = w.answered().as_ref().expect_err("the call is refused");
    assert_eq!(e.code, ErrorCode::Unauthenticated);
    let OpError {
        kind,
        message: said,
        ..
    } = op_error(e).expect("the engine's own error");
    assert_eq!((kind, said), (ErrorKind::Unauthenticated, message));
}

#[when(
    "the client creates a key over Connect whose expiry has 1 nanosecond past a whole millisecond"
)]
async fn too_fine(w: &mut Serve) {
    let mut request = CreateKeyRequest::default();
    let expiry = request.expires_at.get_or_insert_default();
    // Ahead of now by a day, so that nothing but its fraction is wrong.
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    expiry.seconds = i64::try_from(now.as_secs()).unwrap() + 86_400;
    expiry.nanos = 1_000_001;
    let options = bearer(w.key());
    let client = KeyServiceClient::new(HttpClient::plaintext(), w.config().json());
    let answer = client.create_key_with_options(request, options).await;
    w.created = Some(answer.map(connectrpc::UnaryResponse::into_owned));
}

#[then("the call is refused as invalid")]
fn refused_as_invalid(w: &mut Serve) {
    let e = w.refusal();
    assert_eq!(e.code, ErrorCode::InvalidArgument);
    if w.created.is_some() {
        let e = op_error(e).expect("the door's own error");
        let message = "a time is in whole milliseconds and in years 0001 to 9999";
        assert_eq!((e.kind, &*e.message), (ErrorKind::Invalid, message));
    } else {
        // It did not decode, so the library refused it.
        assert_eq!(op_error(e), None);
        let message = "failed to decode proto request: invalid UTF-8 in string field";
        assert_eq!(e.message.as_deref(), Some(message));
    }
}

#[given(
    regex = r#"^the engine is served(?: and|,) a client holds a ("write"|"read"|"write" and "read") key(?: and an observer holds a ("read") key)?$"#
)]
async fn served_with_client(w: &mut Serve, client: String, observer: String) {
    served_with_key(w);
    w.client_key = Some(w.create(&client).await);
    if !observer.is_empty() {
        w.observer_key = Some(w.create(&observer).await);
    }
}

fn named(name: &str) -> EmitRequest {
    EmitRequest {
        name: name.to_owned(),
        ..EmitRequest::default()
    }
}

#[when("the client emits an event over Connect")]
async fn emits(w: &mut Serve) {
    let client = w.events("Connect JSON");
    let answer = client
        .emit_with_options(named("order.placed"), bearer(w.client_key()))
        .await;
    w.emitted = Some(answer.map(|a| a.into_owned().try_into().expect("an answer")));
}

#[then(regex = r#"^the call is refused as permission denied with "([^"]+)"$"#)]
fn permission_denied(w: &mut Serve, message: String) {
    let e = w.refusal();
    assert_eq!(e.code, ErrorCode::PermissionDenied);
    let e = op_error(e).expect("the engine's own error");
    assert_eq!((e.kind, e.message), (ErrorKind::PermissionDenied, message));
}

#[then(regex = r#"^the hint says a key with scope "(\w+)" is needed$"#)]
fn hint_names_scope(w: &mut Serve, scope: String) {
    let e = op_error(w.refusal()).expect("the engine's own error");
    let hint = format!("create a key with scope {scope}, or use one that has it");
    assert_eq!(e.hint, Some(hint));
}

#[then("the error is not retryable")]
fn not_retryable(w: &mut Serve) {
    let e = op_error(w.refusal()).expect("the engine's own error");
    assert!(!e.kind.retryable());
}

/// A data cell is the JSON itself, unless it starts `built:`.
fn data(cell: &str) -> &str {
    match cell {
        "built: an object spread over two lines" => "{\"b\": 1,\n \"a\": 2}",
        cell => {
            assert!(!cell.starts_with("built:"), "{cell}");
            cell
        }
    }
}

#[when(
    regex = r"^the client emits an event with data (.+) over (Connect JSON|Connect binary|gRPC|gRPC-Web)$"
)]
async fn emits_data(w: &mut Serve, cell: String, codec: String) {
    let request = EmitRequest {
        data: Some(data(&cell).to_owned()),
        ..named("order.placed")
    };
    let client = w.events(&codec);
    let answer = client
        .emit_with_options(request, bearer(w.client_key()))
        .await;
    w.emitted = Some(answer.map(|a| a.into_owned().try_into().expect("an answer")));
}

/// The emitted event, read over `codec`.
async fn read(w: &Serve, codec: &str) -> Event {
    let emitted = w.emitted.as_ref().expect("an emit");
    let request = GetEventRequest {
        event_id: emitted.as_ref().expect("an event").event_id.to_string(),
        ..GetEventRequest::default()
    };
    let client = w.events(codec);
    let answer = client
        .get_event_with_options(request, bearer(w.client_key()))
        .await;
    answer.expect("the event").into_owned()
}

#[when(regex = r"^reads it back over (Connect JSON|Connect binary|gRPC|gRPC-Web)$")]
async fn reads_back(w: &mut Serve, codec: String) {
    w.event = Some(read(w, &codec).await);
}

#[then(regex = r"^the data is byte for byte (.+)$")]
fn the_data_is(w: &mut Serve, cell: String) {
    let event = w.event.as_ref().expect("an event");
    assert_eq!(event.data.as_deref(), Some(data(&cell)));
}

#[then("over JSON the event's time is RFC 3339 in UTC, with no fraction finer than a millisecond")]
async fn the_time_is(w: &mut Serve) {
    // As it is sent: a client would read any spelling of the time.
    let emitted = w.emitted.as_ref().expect("an emit");
    let id = emitted.as_ref().expect("an event").event_id;
    let body = format!(r#"{{"event_id":"{id}"}}"#);
    let rpc = Operation::GetEvent.rpc();
    let event = post_json(&w.served().url(), rpc, w.client_key(), &body);
    let time = event["time"].as_str().expect("a time").to_owned();
    // 2027-01-15T08:00:00Z or 2027-01-15T08:00:00.123Z
    let shape: String = time
        .chars()
        .map(|c| if c.is_ascii_digit() { '0' } else { c })
        .collect();
    let whole = "0000-00-00T00:00:00Z";
    let millis = "0000-00-00T00:00:00.000Z";
    assert!([whole, millis].contains(&&*shape), "{time}");
}

#[when("the client sends an emit batch whose request body is 17 MiB")]
async fn sends_17_mib(w: &mut Serve) {
    let size = 17 * 1024 * 1024;
    // An event the engine would store, and spaces.
    let mut body = br#"{"items":[{"name":"a.one"}]}"#.to_vec();
    body.resize(size, b' ');
    // HTTP/1.1 closes the connection on a body it refuses, and can lose the answer with it.
    let http = HttpClient::plaintext_http2_only();
    let client = EventServiceClient::new(WithBody(http, body), w.config().json());
    let answer = client
        .emit_batch_with_options(EmitBatchRequest::default(), bearer(w.client_key()))
        .await;
    w.batch = Some(answer.map(|a| a.into_owned().try_into().expect("an answer")));
}

#[then("the call is refused as resource exhausted")]
fn resource_exhausted(w: &mut Serve) {
    let e = w.refusal();
    assert_eq!(e.code, ErrorCode::ResourceExhausted);
    // By the library, before the engine.
    assert_eq!(op_error(e), None);
    let message = e.message.as_deref();
    assert_eq!(message, Some("request body size exceeds limit 16777216"));
}

#[then("the observer lists no events")]
async fn observer_lists_none(w: &mut Serve) {
    let client = Client::new(&w.served().url()).unwrap();
    let key = w.observer_key.clone().expect("an observer");
    let request = ListEventsRequest {
        limit: None,
        cursor: None,
    };
    let page = client
        .list_events(&Call::with_key(Secret::new(key)), request)
        .await
        .unwrap();
    assert_eq!((page.events, page.next), (vec![], None));
}

#[when(
    regex = r"^the client sends an emit over Connect binary whose (name|idempotency key|data|metadata) holds the bytes FF FE$"
)]
async fn sends_bytes(w: &mut Serve, field: String) {
    let number = match field.as_str() {
        "name" => 1,
        "data" => 2,
        "metadata" => 3,
        _ => 5,
    };
    // A name, then the field, each a tag and a length before its bytes.
    let body = [b"\x0a\x05a.one", &[number << 3 | 2, 2, 0xff, 0xfe][..]].concat();
    let (http, config) = w.transport("Connect binary");
    let client = EventServiceClient::new(WithBody(http, body), config);
    let answer = client
        .emit_with_options(EmitRequest::default(), bearer(w.client_key()))
        .await;
    w.emitted = Some(answer.map(|a| a.into_owned().try_into().expect("an answer")));
}

#[when(
    regex = r#"^the client emits a batch over Connect of an event named "([^"]+)" and an event with no name$"#
)]
async fn emits_batch(w: &mut Serve, name: String) {
    let request = EmitBatchRequest {
        items: vec![named(&name), named("")],
        ..EmitBatchRequest::default()
    };
    let client = w.events("Connect JSON");
    let answer = client
        .emit_batch_with_options(request, bearer(w.client_key()))
        .await;
    w.batch = Some(answer.map(|a| a.into_owned().try_into().expect("an answer")));
}

fn results(w: &Serve) -> &[Result<EmitResponse, OpError>] {
    let batch = w.batch.as_ref().expect("a batch").as_ref();
    let items = &batch.expect("the batch succeeds").items;
    assert_eq!(items.len(), 2, "{items:?}");
    items
}

#[then("the first result is an event")]
fn first_is_event(w: &mut Serve) {
    let first = results(w)[0].as_ref().expect("an event");
    assert!(!first.duplicate);
}

#[then(regex = r#"^the second result is an error of kind invalid with message "([^"]+)"$"#)]
fn second_is_error(w: &mut Serve, message: String) {
    let refused = OpError {
        kind: ErrorKind::Invalid,
        message,
        hint: None,
    };
    assert_eq!(results(w)[1], Err(refused));
}

#[then("listing keys shows only the bootstrap key")]
async fn only_the_bootstrap_key(w: &mut Serve) {
    let client = Client::new(&w.served().url()).unwrap();
    let cx = Call::with_key(Secret::new(w.key().to_owned()));
    let page = client.list_keys(&cx, ListKeysRequest::default()).await;
    let page = page.unwrap();
    let keys: Vec<_> = page.keys.iter().map(|k| (k.key_id, k.origin)).collect();
    let bootstrap = w.key()[4..40].parse().unwrap();
    assert_eq!(keys, [(bootstrap, KeyOrigin::Minted)]);
    assert_eq!(page.next, None);
}

pub async fn run(parser: super::Unescaped) -> (bool, usize) {
    let writer = Serve::cucumber::<&str>()
        .with_parser(parser)
        .fail_on_skipped()
        .filter_run(
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../docs/behaviors/serve.feature"
            ),
            |_, _, scenario| {
                let registered = |tag: &String| REGISTERED.contains(&tag.as_str());
                scenario.tags.iter().any(registered)
            },
        )
        .await;
    let passed = writer.scenarios_stats().passed;
    (writer.execution_has_failed(), passed)
}
