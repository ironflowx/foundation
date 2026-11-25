//! `cli.feature` on the `ironflowx` binary: a served engine, and each command a process of its own.

use std::collections::BTreeMap;
use std::fmt;
use std::process::{Output, Stdio};

use cucumber::writer::Stats as _;
use cucumber::{World, given, then, when};
use serde_json::{Value, json};
use tempfile::TempDir;

use super::serve::{Served, command};

/// The scenarios whose work has landed: every one must run and pass.
const REGISTERED: &[&str] = &["CLI-002", "CLI-004", "CLI-007", "CLI-016"];

/// How many scenarios run, an outline once per example, so no example goes unnoticed.
pub const RUNS: usize = 4;

#[derive(World)]
#[world(init = Self::new)]
pub struct Cli {
    served: Option<Served>,
    /// The working directory, which holds the data directory.
    dir: TempDir,
    variables: BTreeMap<&'static str, String>,
    ran: Option<Output>,
}

/// The default: where a CLI that looked for an admin key would look.
const DATA_DIR: &str = ".ironflowx";

impl fmt::Debug for Cli {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Cli")
            .field("ran", &self.ran)
            .finish_non_exhaustive()
    }
}

impl Cli {
    fn new() -> Self {
        Self {
            served: None,
            dir: tempfile::tempdir().unwrap(),
            variables: BTreeMap::new(),
            ran: None,
        }
    }

    fn ran(&self) -> &Output {
        self.ran.as_ref().expect("a command ran")
    }

    fn stdout(&self) -> &str {
        std::str::from_utf8(&self.ran().stdout).unwrap()
    }

    fn stderr(&self) -> &str {
        std::str::from_utf8(&self.ran().stderr).unwrap()
    }

    /// The bootstrap key, in `admin.key`.
    fn key(&self) -> String {
        let path = self.dir.path().join(DATA_DIR).join("admin.key");
        let text = std::fs::read_to_string(path).unwrap();
        text.strip_suffix('\n').expect("a newline").to_owned()
    }

    /// Stdout, as the one JSON line it must be.
    fn answer(&self) -> Value {
        let line = self.stdout().strip_suffix('\n').expect("a line");
        assert!(!line.contains('\n'), "{line}");
        serde_json::from_str(line).expect("one JSON value")
    }

    /// The envelope of the command that failed, with nothing on stdout.
    fn envelope(&self) -> Value {
        assert_eq!(self.stdout(), "");
        let line = self.stderr().strip_suffix('\n').expect("a line");
        assert!(!line.contains('\n'), "{line}");
        serde_json::from_str(line).expect("one JSON value")
    }
}

#[given("the engine is served on a fresh data directory")]
fn served(w: &mut Cli) {
    let served = Served::start(&w.dir.path().join(DATA_DIR), "127.0.0.1:0");
    served.ready();
    w.served = Some(served);
}

#[given("the CLI is pointed at it with IRONFLOWX_URL")]
fn pointed(w: &mut Cli) {
    let url = w.served.as_ref().expect("a served engine").url();
    w.variables.insert("IRONFLOWX_URL", url);
}

/// Every command drops them.
#[given("no other IRONFLOWX_ variable is set except those the scenario names")]
fn no_variables(_: &mut Cli) {}

#[given("IRONFLOWX_KEY holds the bootstrap key")]
fn bootstrap_key(w: &mut Cli) {
    w.variables.insert("IRONFLOWX_KEY", w.key());
}

#[given("no key is given")]
fn no_key(w: &mut Cli) {
    assert!(!w.variables.contains_key("IRONFLOWX_KEY"));
}

#[given("IRONFLOWX_URL names an address where nothing listens")]
fn nothing_listens(w: &mut Cli) {
    // A port the system gives to no one who asks for any.
    let url = "http://127.0.0.1:1".to_owned();
    w.variables.insert("IRONFLOWX_URL", url);
}

#[given(
    regex = r#"^the working directory is the one holding the data directory, with "admin.key" in it$"#
)]
fn beside_the_key(w: &mut Cli) {
    // In either one.
    std::fs::write(w.dir.path().join("admin.key"), w.key()).unwrap();
}

#[when(regex = r#"^"ironflowx ([^"]*)" runs(?: with stdout piped)?$"#)]
fn runs(w: &mut Cli, args: String) {
    let out = command()
        .args(args.split(' '))
        .current_dir(w.dir.path())
        .envs(&w.variables)
        .stdin(Stdio::null())
        .output();
    w.ran = Some(out.unwrap());
}

#[then("stdout is exactly one line, a JSON object naming the key's ID and scopes")]
fn names_the_key(w: &mut Cli) {
    let expected = json!({
        "key_id": w.key()[4..40],
        "scopes": ["read", "write", "admin"],
        "environment": "default/default",
    });
    assert_eq!(w.answer(), expected);
    assert_eq!(w.ran().status.code(), Some(0));
}

#[then("stderr is empty")]
fn no_stderr(w: &mut Cli) {
    assert_eq!(w.stderr(), "");
}

#[then(expr = "it exits with code {int} and an envelope with code {string} and retryable true")]
fn retryable(w: &mut Cli, exit: i32, code: String) {
    let envelope = w.envelope();
    assert_eq!(
        (&envelope["code"], &envelope["retryable"]),
        (&json!(code), &json!(true))
    );
    assert_eq!(w.ran().status.code(), Some(exit));
}

#[then(expr = "it exits with code {int} and an envelope with code {string}")]
fn fails(w: &mut Cli, exit: i32, code: String) {
    assert_eq!(w.envelope()["code"], code);
    assert_eq!(w.ran().status.code(), Some(exit));
}

#[then(
    regex = r#"^stdout is a JSON object with "version", "commit" and "protocol" "ironflowx.v1"$"#
)]
fn version(w: &mut Cli) {
    let answer = w.answer();
    let text = |name: &str| !answer[name].as_str().expect("a string").is_empty();
    assert!(text("version") && text("commit"), "{answer}");
    assert_eq!(answer["protocol"], "ironflowx.v1");
    assert_eq!(answer.as_object().expect("an object").len(), 3);
    assert_eq!(w.ran().status.code(), Some(0));
}

pub async fn run(parser: super::Unescaped) -> (bool, usize) {
    let writer = Cli::cucumber::<&str>()
        .with_parser(parser)
        .fail_on_skipped()
        .filter_run(
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../docs/behaviors/cli.feature"
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
