//! `serve.feature` on the `ironflowx` binary, started by each scenario.

use std::fmt;
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::TcpStream;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use connectrpc::client::{
    BoxFuture, CallOptions, ClientBody, ClientConfig, ClientTransport, HttpClient,
};
use connectrpc::{ConnectError, ErrorCode, Protocol};
use cucumber::writer::Stats as _;
use cucumber::{World, given, then, when};
use ironflowx_core::{ErrorKind, Scope, Secret};
use ironflowx_proto::wire::{IdentityServiceClient, WhoamiRequest, WhoamiResponse, op_error};
use ironflowx_proto::{Call, Client, OpError, Operations as _};
use serde_json::Value;
use tempfile::TempDir;

/// The scenarios whose work has landed: every one must run and pass.
const REGISTERED: &[&str] = &["HOST-001", "HOST-016", "HOST-021", "HOST-027"];

/// How many scenarios run, an outline once per example, so no example goes unnoticed.
pub const RUNS: usize = 14;

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

/// A started `ironflowx serve`, killed when dropped, and what it has logged.
struct Served {
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
}

impl Served {
    fn start(data_dir: &Path, listen: &str) -> Self {
        let mut command = Command::new(BINARY.get().expect("the binary is built"));
        for (name, _) in std::env::vars_os() {
            if name.to_string_lossy().starts_with("IRONFLOWX_") {
                command.env_remove(name);
            }
        }
        let mut child = command
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

    fn url(&self) -> String {
        until("listening", || self.listening().into_iter().next())
    }

    /// Waits until `/ready` answers 200.
    fn ready(&self) {
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

#[derive(World)]
#[world(init = Self::new)]
pub struct Serve {
    served: Option<Served>,
    dir: TempDir,
    /// The admin key's text.
    key: Option<String>,
    answer: Option<Result<WhoamiResponse, ConnectError>>,
}

impl fmt::Debug for Serve {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Serve")
            .field("log", &self.served.as_ref().map(Served::log))
            .field("answer", &self.answer)
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

impl Serve {
    fn new() -> Self {
        Self {
            served: None,
            dir: tempfile::tempdir().unwrap(),
            key: None,
            answer: None,
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
fn served_with_key(w: &mut Serve) {
    w.serve("127.0.0.1:0");
    w.served().ready();
    w.read_key();
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
    let path = path.to_str().unwrap();
    until("logged", || w.served().log().contains(path).then_some(()));
    let log = w.served().log();
    assert!(!log.contains(&w.key()[41..]), "{log}");
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
    assert!(!log.contains(&w.key()[41..]), "{log}");
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
    let (http, config) = match protocol.as_str() {
        "Connect JSON" => (HttpClient::plaintext(), w.config().json()),
        "Connect binary" => (HttpClient::plaintext(), w.config().proto()),
        "gRPC" => (
            HttpClient::plaintext_http2_only(),
            w.config().with_protocol(Protocol::Grpc),
        ),
        "gRPC-Web" => (
            HttpClient::plaintext(),
            w.config().with_protocol(Protocol::GrpcWeb),
        ),
        _ => unreachable!(),
    };
    let bearer = format!("Bearer {}", w.key());
    let options = CallOptions::default().with_header("authorization", bearer);
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
