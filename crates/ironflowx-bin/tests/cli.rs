//! The CLI, as a process, calling the HTTP door of a recording `Operations`.

#[path = "../../ironflowx-doors/tests/common/mod.rs"]
#[allow(dead_code)]
mod common;

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs::File;
use std::io::{Read as _, Write as _};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::os::unix::ffi::OsStrExt as _;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::sync::Arc;

use common::{KEY, Recording, serving, whoami};
use ironflowx_core::{Address, ErrorKind};
use ironflowx_proto::{OpError, Whoami};
use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};
use serde_json::{Value, json};

/// The CLI, with no variable of ours set.
fn command(args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ironflowx"));
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("IRONFLOWX_") {
            command.env_remove(name);
        }
    }
    command.args(args).stdin(Stdio::null());
    command
}

fn stdout(out: &Output) -> &str {
    std::str::from_utf8(&out.stdout).unwrap()
}

/// The envelope of a failure: the one line of stderr, with nothing on stdout, and its exit code.
fn envelope(out: &Output) -> (Value, i32) {
    let stderr = std::str::from_utf8(&out.stderr).unwrap();
    assert_eq!(stdout(out), "", "{stderr}");
    let line = stderr.strip_suffix('\n').expect("a line");
    assert!(!line.contains('\n'), "{stderr}");
    let envelope = serde_json::from_str(line).expect("one JSON value");
    (envelope, out.status.code().expect("it exits"))
}

fn invalid(message: &str) -> (Value, i32) {
    let envelope = json!({"code": "invalid", "message": message, "retryable": false});
    (envelope, 2)
}

/// As `invalid`, of a command line that could not be read.
fn unread(message: &str) -> (Value, i32) {
    let (mut envelope, code) = invalid(message);
    envelope["hint"] = json!("--help says what a command takes");
    (envelope, code)
}

/// A server that answers `answer`, its URL, and the runtime it lives on.
struct Served {
    url: String,
    operations: Arc<Recording>,
    _runtime: tokio::runtime::Runtime,
}

impl Served {
    fn answering(answer: Result<Whoami, OpError>) -> Self {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (addr, operations): (SocketAddr, _) = runtime.block_on(serving(answer));
        Self {
            url: format!("http://{addr}"),
            operations,
            _runtime: runtime,
        }
    }

    fn answers(&self, answer: Result<Whoami, OpError>) {
        *self.operations.answer.lock().unwrap() = answer;
    }

    fn whoami(&self) -> Command {
        let mut command = command(&["whoami"]);
        command.env("IRONFLOWX_URL", &self.url);
        command
    }

    /// Runs `command`, which must succeed: the key it sent.
    fn key_sent(&self, command: &mut Command) -> Option<String> {
        let out = command.output().unwrap();
        assert_eq!(out.status.code(), Some(0), "{out:?}");
        let mut keys = self.operations.take();
        assert_eq!(keys.len(), 1);
        keys.remove(0)
    }
}

/// D65: no field an agent must guess at, and one line.
#[test]
fn every_field_that_is_not_optional_is_printed() {
    let served = Served::answering(Ok(whoami()));
    let out = served.whoami().output().unwrap();
    assert_eq!(
        stdout(&out),
        concat!(
            r#"{"environment":"default/main","expires_at":"2027-01-15T08:00:00.123Z","#,
            r#""key_id":"0190b5f0-0000-7000-8000-000000000001","scopes":["read","admin"]}"#,
            "\n"
        )
    );
    assert_eq!((out.status.code(), out.stderr), (Some(0), vec![]));
    served.answers(Ok(Whoami {
        scopes: BTreeSet::new(),
        expires_at: None,
        ..whoami()
    }));
    let out = served.whoami().output().unwrap();
    assert_eq!(
        stdout(&out),
        concat!(
            r#"{"environment":"default/main","#,
            r#""key_id":"0190b5f0-0000-7000-8000-000000000001","scopes":[]}"#,
            "\n"
        )
    );
    assert_eq!((out.status.code(), out.stderr), (Some(0), vec![]));
}

#[test]
fn a_terminal_gets_indented_output() {
    let served = Served::answering(Ok(whoami()));
    let terminal = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY).unwrap();
    grantpt(&terminal).unwrap();
    unlockpt(&terminal).unwrap();
    let name = ptsname(&terminal, Vec::new()).unwrap();
    let end = File::options()
        .read(true)
        .write(true)
        .open(OsStr::from_bytes(name.as_bytes()))
        .unwrap();
    let mut command = served.whoami();
    command.stdout(end).stderr(Stdio::piped());
    let mut child = command.spawn().unwrap();
    // Its copy of the terminal's end.
    drop(command);
    let mut terminal = File::from(terminal);
    let mut printed = Vec::new();
    let mut buffer = [0; 1024];
    // Linux fails the read once no one holds the other end.
    while let Ok(n @ 1..) = terminal.read(&mut buffer) {
        printed.extend_from_slice(&buffer[..n]);
    }
    let mut stderr = String::new();
    let mut piped = child.stderr.take().unwrap();
    piped.read_to_string(&mut stderr).unwrap();
    assert_eq!((child.wait().unwrap().code(), &*stderr), (Some(0), ""));
    let expected = r#"{
  "environment": "default/main",
  "expires_at": "2027-01-15T08:00:00.123Z",
  "key_id": "0190b5f0-0000-7000-8000-000000000001",
  "scopes": [
    "read",
    "admin"
  ]
}
"#;
    // As a terminal's end takes a newline.
    let printed = String::from_utf8(printed).unwrap();
    assert_eq!(printed, expected.replace('\n', "\r\n"));
}

/// SRV-11, D64: the engine's error, its kind's code, `retryable` and exit code.
#[test]
fn every_kind_is_one_envelope_and_one_exit_code() {
    let served = Served::answering(Ok(whoami()));
    for (kind, code, retryable, exit) in [
        (ErrorKind::Internal, "internal", false, 1),
        (ErrorKind::Invalid, "invalid", false, 2),
        (ErrorKind::Unauthenticated, "unauthenticated", false, 3),
        (ErrorKind::PermissionDenied, "permission_denied", false, 4),
        (ErrorKind::NotFound, "not_found", false, 5),
        (ErrorKind::Unknown, "unknown", false, 6),
        (ErrorKind::ResourceExhausted, "resource_exhausted", false, 7),
        (ErrorKind::Unavailable, "unavailable", true, 8),
    ] {
        for hint in [None, Some("do\nthis")] {
            served.answers(Err(OpError {
                kind,
                message: "two\n\"lines\"".into(),
                hint: hint.map(str::to_owned),
            }));
            let mut expected =
                json!({"code": code, "message": "two\n\"lines\"", "retryable": retryable});
            if let Some(hint) = hint {
                expected["hint"] = json!(hint);
            }
            let out = served.whoami().output().unwrap();
            assert_eq!(envelope(&out), (expected, exit));
        }
    }
}

/// TEN-3: what answers may be no engine, and repeat what it was sent.
#[test]
fn a_key_an_answer_repeats_is_not_printed() {
    let served = Served::answering(Ok(whoami()));
    let other = KEY.replace('A', "B");
    // As one in a file of variables can be.
    let quoted = format!("'{KEY}'");
    let (listed, joined, longer) = (
        format!("{other},{KEY}"),
        format!("{other}{KEY}"),
        format!("{KEY}x"),
    );
    for (sent, repeated, hidden) in [
        (KEY, KEY, true),
        // Its secret alone, all but the last of it, and one less than that.
        (KEY, &KEY[41..], true),
        (KEY, &KEY[..83], true),
        (KEY, &KEY[42..], false),
        // Its ID is no secret.
        (KEY, &KEY[4..40], false),
        (&*quoted, KEY, true),
        (&longer, &KEY[41..], true),
        (&listed, KEY, true),
        (&joined, &KEY[41..], true),
        (&joined, &other[41..], true),
        (&KEY[1..], &KEY[1..], true),
        (&KEY[..43], &KEY[..43], true),
        // Too short to hold a secret, so any message may hold it.
        (&KEY[..42], &KEY[..42], false),
        ("ixk_", "ixk_", false),
        ("e", "e", false),
        ("", "", false),
    ] {
        let mut command = served.whoami();
        command.env("IRONFLOWX_KEY", sent);
        let mut output = || command.output().unwrap();
        // A message or a hint, whichever repeats it.
        for (in_message, in_hint) in [(true, true), (true, false), (false, true)] {
            let text = |repeats| format!("no '{}'", if repeats { repeated } else { "key" });
            let printed = |repeats| match repeats && hidden {
                true => "not printed, since it repeats the key the CLI was given".to_owned(),
                false => text(repeats),
            };
            served.answers(Err(OpError {
                kind: ErrorKind::Unauthenticated,
                message: text(in_message),
                hint: Some(text(in_hint)),
            }));
            let expected = json!({
                "code": "unauthenticated",
                "message": printed(in_message),
                "hint": printed(in_hint),
                "retryable": false,
            });
            let given = format!("{sent} sent, {repeated} repeated");
            assert_eq!(envelope(&output()), (expected, 3), "{given}");
        }
        // Nor in an answer, whose one free text is the environment's name.
        served.answers(Ok(Whoami {
            environment: Address::new("default", &format!("a{repeated}")),
            ..whoami()
        }));
        let out = output();
        let given = format!("{sent} sent, {repeated} repeated");
        if hidden {
            let message = "the answer is not printed, since it repeats the key the CLI was given";
            let expected = json!({"code": "internal", "message": message, "retryable": false});
            assert_eq!(envelope(&out), (expected, 1), "{given}");
        } else {
            let answer: Value = serde_json::from_str(stdout(&out)).unwrap();
            let environment = format!("default/a{repeated}");
            assert_eq!(answer["environment"], environment, "{given}");
        }
    }
}

#[test]
fn a_usage_error_is_an_envelope() {
    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().join("data");
    let own = format!("'{}", &KEY[42..]);
    for (args, message) in [
        (
            &[][..],
            "'ironflowx' requires a subcommand but one was not provided \
             [subcommands: serve, version, whoami]",
        ),
        (&["nope"], "unrecognized subcommand 'nope'"),
        (&["help"], "unrecognized subcommand 'help'"),
        (&["--version"], "unexpected argument '--version' found"),
        (
            &["whoami", "--no-such-flag"],
            "unexpected argument '--no-such-flag' found",
        ),
        (&["whoami", "x"], "unexpected argument 'x' found"),
        (
            &["whoami", "--url"],
            "a value is required for '--url <URL>' but none was supplied",
        ),
        (
            &["whoami", "--url", "http://h", "--url", "http://h"],
            "the argument '--url <URL>' cannot be used multiple times",
        ),
        (&["whoami", KEY], "unexpected argument <hidden> found"),
        (&[KEY], "unrecognized subcommand <hidden>"),
        (
            &["serve", "--listen", KEY],
            "invalid value <hidden> for '--listen <ADDRESS>': invalid socket address syntax",
        ),
        // What a key ends with: its secret, or that and the ID before it.
        (
            &["whoami", &KEY[41..]],
            "unexpected argument <hidden> found",
        ),
        (
            &["whoami", &KEY[42..]],
            "unexpected argument 'AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA' found",
        ),
        (&["whoami", &KEY[4..]], "unexpected argument <hidden> found"),
        (&["whoami", "ixk_x"], "unexpected argument <hidden> found"),
        // Clap's quotes do not count; its own do.
        (&["whoami", &own], "unexpected argument <hidden> found"),
        // An ID is no secret.
        (
            &["whoami", &KEY[4..40]],
            "unexpected argument '0190b5f0-0000-7000-8000-000000000001' found",
        ),
        (
            &["whoami", "--key=ixk_x"],
            "unexpected argument '--key' found",
        ),
        (
            &["version", "--url", "http://h"],
            "unexpected argument '--url' found",
        ),
        (
            &["serve", "--listen", "x"],
            "invalid value 'x' for '--listen <ADDRESS>': invalid socket address syntax",
        ),
    ] {
        let mut command = command(args);
        command.current_dir(dir.path());
        assert_eq!(envelope(&command.output().unwrap()), unread(message));
    }
    assert!(!data_dir.exists() && !dir.path().join(".ironflowx").exists());
}

#[test]
fn help_is_text() {
    for (args, usage) in [
        (&["--help"][..], "Usage: ironflowx <COMMAND>"),
        (&["-h"], "Usage: ironflowx <COMMAND>"),
        (&["whoami", "--help"], "Usage: ironflowx whoami [OPTIONS]"),
        (&["version", "--help"], "Usage: ironflowx version"),
        (&["serve", "--help"], "Usage: ironflowx serve [OPTIONS]"),
    ] {
        let out = command(args).env("IRONFLOWX_KEY", KEY).output().unwrap();
        assert!(stdout(&out).contains(usage), "{}", stdout(&out));
        assert!(!stdout(&out).contains(KEY));
        assert_eq!((out.status.code(), out.stderr), (Some(0), vec![]));
    }
}

/// D61: `--key-file`, then `IRONFLOWX_KEY`, then none.
#[test]
fn the_key_is_the_one_given_and_no_other() {
    let served = Served::answering(Ok(whoami()));
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("key");
    let other = KEY.replace('A', "B");
    let given = |file_holds: Option<&[u8]>, variable: Option<&[u8]>| {
        let mut command = served.whoami();
        command.current_dir(dir.path());
        if let Some(text) = file_holds {
            std::fs::write(&file, text).unwrap();
            command.arg("--key-file").arg(&file);
        }
        if let Some(text) = variable {
            command.env("IRONFLOWX_KEY", OsStr::from_bytes(text));
        }
        served.key_sent(&mut command)
    };
    let (key, spaced) = (KEY.as_bytes(), format!(" \t{KEY}\r\n\n"));
    let spaced = spaced.as_bytes();
    let of = |key: &str| Some(key.to_owned());
    assert_eq!(given(None, None), None);
    assert_eq!(given(None, Some(key)), of(KEY));
    assert_eq!(given(Some(key), None), of(KEY));
    assert_eq!(given(Some(key), Some(other.as_bytes())), of(KEY));
    assert_eq!(given(None, Some(spaced)), of(KEY));
    assert_eq!(given(Some(spaced), None), of(KEY));
    // An empty key is no key, and the flag still wins.
    assert_eq!(given(None, Some(b"")), None);
    assert_eq!(given(None, Some(b" \n")), None);
    assert_eq!(given(Some(b"\n"), Some(key)), None);
    // Not text: no well-formed key, which the engine says.
    let broken = [key, b"\xff"].concat();
    assert_eq!(given(None, Some(&broken)), of("malformed"));
    assert_eq!(given(Some(&broken), None), of("malformed"));
    // Inside, whitespace is part of it.
    assert_eq!(given(None, Some(b"a b")), of("malformed"));

    // Never unasked.
    std::fs::create_dir(dir.path().join(".ironflowx")).unwrap();
    for path in ["admin.key", ".ironflowx/admin.key"] {
        std::fs::write(dir.path().join(path), KEY).unwrap();
    }
    assert_eq!(given(None, None), None);
}

#[test]
fn a_key_file_that_cannot_be_read_is_a_usage_error() {
    let served = Served::answering(Ok(whoami()));
    let dir = tempfile::tempdir().unwrap();
    for (path, why) in [
        (dir.path(), "Is a directory (os error 21)"),
        // A key given where its file's path goes.
        (Path::new(KEY), "No such file or directory (os error 2)"),
    ] {
        let mut command = served.whoami();
        command
            .env("IRONFLOWX_KEY", KEY)
            .arg("--key-file")
            .arg(path);
        let message = format!("cannot read the file --key-file names: {why}");
        assert_eq!(envelope(&command.output().unwrap()), invalid(&message));
    }
    assert!(served.operations.take().is_empty());
}

#[test]
fn the_engine_is_the_one_the_flag_or_else_the_variable_names() {
    let served = Served::answering(Ok(whoami()));
    let mut by_flag = command(&["whoami", "--url", &served.url]);
    by_flag.env("IRONFLOWX_URL", "http://127.0.0.1:1");
    assert_eq!(served.key_sent(&mut by_flag), None);
    assert_eq!(served.key_sent(&mut served.whoami()), None);

    let form = "a URL of the form http://host[:port][/path]";
    for given in [
        &b""[..],
        b"127.0.0.1:9310",
        b"http://u:pw@h",
        b"http://h/\xff",
    ] {
        let given = OsStr::from_bytes(given);
        let mut variable = command(&["whoami"]);
        variable.env("IRONFLOWX_URL", given);
        let message = format!("IRONFLOWX_URL is not {form}");
        assert_eq!(envelope(&variable.output().unwrap()), invalid(&message));
    }
    let mut flag = command(&["whoami", "--url", "http://u:pw@h"]);
    flag.env("IRONFLOWX_URL", &served.url);
    let message = format!("--url is not {form}");
    assert_eq!(envelope(&flag.output().unwrap()), invalid(&message));
    assert!(served.operations.take().is_empty());
}

/// The client's own tests show that it holds to the deadline it sends.
#[test]
fn a_call_is_given_30_seconds() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let url = format!("http://{addr}");
    let head = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            if stream.read(&mut byte).unwrap() == 0 {
                break;
            }
            head.extend(byte);
        }
        let _ = stream.write_all(b"HTTP/1.1 503 x\r\n\r\n");
        String::from_utf8(head).unwrap().to_lowercase()
    });
    let out = command(&["whoami", "--url", &url]).output().unwrap();
    // If it never called, this ends the wait.
    drop(TcpStream::connect(addr));
    let head = head.join().unwrap();
    assert!(head.contains("\r\nconnect-timeout-ms: 30000\r\n"), "{head}");
    let (envelope, exit) = envelope(&out);
    assert_eq!((&envelope["code"], exit), (&json!("unavailable"), 8));
}

#[test]
fn an_answer_that_cannot_be_printed_is_a_failure() {
    let served = Served::answering(Ok(whoami()));
    for mut command in [served.whoami(), command(&["version"]), command(&["--help"])] {
        // A pipe nothing reads.
        let (reader, writer) = std::io::pipe().unwrap();
        drop(reader);
        let child = command.stdout(writer).stderr(Stdio::piped()).spawn();
        drop(command);
        let (envelope, exit) = envelope(&child.unwrap().wait_with_output().unwrap());
        let message = "cannot write to stdout: Broken pipe (os error 32)";
        let expected = json!({"code": "internal", "message": message, "retryable": false});
        assert_eq!((envelope, exit), (expected, 1));
    }
}

#[test]
fn version_names_the_commit_it_was_built_from() {
    let head = Command::new("git").args(["rev-parse", "HEAD"]).output();
    let head = head.ok().filter(|out| out.status.success());
    let head = head.map(|out| String::from_utf8(out.stdout).unwrap().trim().to_owned());
    let out = command(&["version"]).output().unwrap();
    let version: Value = serde_json::from_str(stdout(&out)).unwrap();
    let expected = json!({
        "version": env!("CARGO_PKG_VERSION"),
        "commit": head.unwrap_or_else(|| "unknown".to_owned()),
        "protocol": "ironflowx.v1",
    });
    assert_eq!(version, expected);
    assert_eq!((out.status.code(), out.stderr), (Some(0), vec![]));
}
