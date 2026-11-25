//! The CLI: an engine's operations as commands, generated from the proto, and `version`
//! (architecture.md, "CLI"). An answer is JSON on stdout; a failure is one envelope on stderr and
//! its kind's exit code (D64, D65).

#![forbid(unsafe_code)]

use std::ffi::OsString;
use std::io::{self, IsTerminal as _, Write as _};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::{Arg, ArgMatches, value_parser};
use ironflowx_core::{ErrorKind, Secret};
use ironflowx_proto::{Call, Client, OpError, wire};
use serde_json::{Value, json};

/// The most a call takes.
const DEADLINE: Duration = Duration::from_secs(30);

/// What every key starts with.
const KEY_PREFIX: &str = "ixk_";

/// A command the generator found in the proto.
struct Command {
    /// Its words, as `events emit`.
    path: &'static str,
    summary: &'static str,
    /// The answer's fields that need something before they are printed.
    answer: &'static [Field],
}

struct Field {
    name: &'static str,
    /// The JSON text of its default, which wire JSON leaves out; `None`: an absent one stays so.
    default: Option<&'static str>,
    /// The fields of the message, or of each of the messages, it holds.
    fields: &'static [Field],
}

mod generated {
    use ironflowx_proto::{Call, Client, OpError, Operations as _, wire};

    use super::{Command, Field};

    include!(concat!(env!("OUT_DIR"), "/cli.rs"));
}

/// Parses the process's arguments as one of `root`'s commands or of the CLI's own.
///
/// # Errors
///
/// What the process exits with: `--help` has printed its text, or a usage error its envelope.
pub fn parse(root: clap::Command) -> Result<ArgMatches, ExitCode> {
    let root = root.subcommand(
        clap::Command::new("version").about("Show the version, the commit and the protocol"),
    );
    let root = generated::COMMANDS.iter().fold(root, |root, command| {
        let words: Vec<_> = command.path.split(' ').collect();
        add(root, &words, command)
    });
    group(root).try_get_matches().map_err(|e| {
        if !e.use_stderr() {
            return end(e.print().map_err(|e| unprinted(&e)));
        }
        // Its first paragraph; the rest is the usage.
        let text = e.render().to_string();
        let first = text.split("\n\n").next().unwrap_or_default();
        // A key given where none goes is not printed (TEN-3).
        let hide = |word| {
            if str::contains(word, KEY_PREFIX) {
                "'<key>'"
            } else {
                word
            }
        };
        let words: Vec<_> = first.split_whitespace().map(hide).collect();
        let message = words.join(" ");
        end(Err(OpError {
            hint: Some("--help says what a command takes".to_owned()),
            ..invalid(message.trim_start_matches("error: "))
        }))
    })
}

/// Runs the CLI's own command that [`parse`] found: the process's exit code.
pub fn run(matches: &ArgMatches) -> ExitCode {
    let mut words = Vec::new();
    let mut leaf = matches;
    while let Some((word, inner)) = leaf.subcommand() {
        words.push(word);
        leaf = inner;
    }
    end(answer(&words.join(" "), leaf).and_then(|a| print(&a)))
}

/// A command that is nothing without one of its subcommands.
fn group(command: clap::Command) -> clap::Command {
    command
        .subcommand_required(true)
        // `--help` is the one answer that is not JSON.
        .disable_help_subcommand(true)
}

fn add(parent: clap::Command, words: &[&'static str], command: &Command) -> clap::Command {
    let [word, rest @ ..] = words else {
        unreachable!("a command has a word");
    };
    if rest.is_empty() {
        return parent.subcommand(
            clap::Command::new(*word)
                .about(command.summary)
                .arg(
                    Arg::new("url")
                        .value_name("URL")
                        .long("url")
                        .help("The engine to call; else IRONFLOWX_URL, else http://127.0.0.1:9310"),
                )
                .arg(
                    Arg::new("key-file")
                        .long("key-file")
                        .value_name("PATH")
                        .help("The file that holds the key; else IRONFLOWX_KEY holds it")
                        .value_parser(value_parser!(PathBuf)),
                ),
        );
    }
    let parent = match parent.find_subcommand(word) {
        Some(_) => parent,
        None => parent.subcommand(group(clap::Command::new(*word))),
    };
    parent.mut_subcommand(word, |group| add(group, rest, command))
}

fn answer(command: &str, matches: &ArgMatches) -> Result<Value, OpError> {
    if command == "version" {
        return Ok(json!({
            "version": env!("CARGO_PKG_VERSION"),
            "commit": env!("IRONFLOWX_COMMIT"),
            "protocol": "ironflowx.v1",
        }));
    }
    let client = client(matches)?.with_timeout(DEADLINE);
    let cx = credential(matches)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| internal(format!("no runtime: {e}")))?;
    let mut answer = runtime
        .block_on(generated::call(command, &client, &cx))
        .map_err(|e| without_key(e, &cx))?;
    let known = generated::COMMANDS.iter().find(|c| c.path == command);
    complete(&mut answer, known.expect("a command it called").answer);
    Ok(answer)
}

fn client(matches: &ArgMatches) -> Result<Client, OpError> {
    let flag = matches.get_one::<String>("url").cloned();
    let (given, url) = url(flag, std::env::var_os("IRONFLOWX_URL"));
    Client::new(&url).map_err(|e| invalid(format!("{given} is {e}")))
}

/// The URL to call, and what gave it.
fn url(flag: Option<String>, variable: Option<OsString>) -> (&'static str, String) {
    match (flag, variable) {
        (Some(url), _) => ("--url", url),
        // Text that is not UTF-8 is no URL; nor is an empty one.
        (None, Some(url)) => ("IRONFLOWX_URL", url.into_string().unwrap_or_default()),
        (None, None) => ("the default", "http://127.0.0.1:9310".to_owned()),
    }
}

/// The key given, and no other: never `admin.key` (D61).
fn credential(matches: &ArgMatches) -> Result<Call, OpError> {
    let key = if let Some(path) = matches.get_one::<PathBuf>("key-file") {
        // Without the path, which may be a key given by mistake.
        std::fs::read(path)
            .map_err(|e| invalid(format!("cannot read the file --key-file names: {e}")))?
    } else if let Some(key) = std::env::var_os("IRONFLOWX_KEY") {
        key.into_encoded_bytes()
    } else {
        return Ok(Call::anonymous());
    };
    // Text that is not UTF-8 is no well-formed key; nor is what replaces it.
    let key = String::from_utf8_lossy(&key).trim().to_owned();
    Ok(Call::with_key(Secret::new(key)))
}

/// `e`, whose text may come from no engine, without the key it was sent (TEN-3).
fn without_key(mut e: OpError, cx: &Call) -> OpError {
    let key = cx.key().map(Secret::expose);
    // Any other text is no one's key, and may be in any message.
    if let Some(key) = key.filter(|k| k.contains(KEY_PREFIX)) {
        e.message = e.message.replace(key, "<key>");
        e.hint = e.hint.map(|hint| hint.replace(key, "<key>"));
    }
    e
}

/// Adds the fields that hold their defaults, which wire JSON leaves out, so an absent one is always
/// one with presence (D65).
fn complete(value: &mut Value, fields: &[Field]) {
    for field in fields {
        if let (Some(default), Some(object)) = (field.default, value.as_object_mut()) {
            let default = || serde_json::from_str(default).expect("the generator writes JSON");
            object.entry(field.name).or_insert_with(default);
        }
        match value.get_mut(field.name) {
            Some(Value::Array(items)) => {
                items
                    .iter_mut()
                    .for_each(|item| complete(item, field.fields));
            }
            Some(inner) => complete(inner, field.fields),
            None => {}
        }
    }
}

fn print(answer: &Value) -> Result<(), OpError> {
    let mut out = io::stdout().lock();
    let text = if out.is_terminal() {
        format!("{answer:#}")
    } else {
        answer.to_string()
    };
    writeln!(out, "{text}")
        .and_then(|()| out.flush())
        .map_err(|e| unprinted(&e))
}

/// The exit code of an answer printed, or of a failure, once its envelope is.
fn end(result: Result<(), OpError>) -> ExitCode {
    let Err(e) = result else {
        return ExitCode::SUCCESS;
    };
    let mut envelope = json!({
        "code": wire::ErrorInfo::from(&e).kind,
        "message": e.message,
        "retryable": e.kind.retryable(),
    });
    if let Some(hint) = e.hint {
        envelope["hint"] = hint.into();
    }
    // The exit code still says what happened.
    let _ = writeln!(io::stderr(), "{envelope}");
    ExitCode::from(e.kind.exit_code())
}

fn invalid(message: impl Into<String>) -> OpError {
    OpError {
        kind: ErrorKind::Invalid,
        message: message.into(),
        hint: None,
    }
}

fn internal(message: String) -> OpError {
    OpError {
        kind: ErrorKind::Internal,
        message,
        hint: None,
    }
}

fn unprinted(e: &io::Error) -> OpError {
    internal(format!("cannot write to stdout: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_is_found_under_its_words() {
        let command = |path| Command {
            path,
            summary: "",
            answer: &[],
        };
        let root = ["whoami", "keys list", "keys revoke", "a b c"]
            .into_iter()
            .fold(clap::Command::new("ironflowx"), |root, path| {
                let words: Vec<_> = path.split(' ').collect();
                add(root, &words, &command(path))
            });
        let root = group(root);
        root.clone().debug_assert();
        for (args, ok) in [
            ("whoami", true),
            ("keys list --url x --key-file y", true),
            ("keys revoke", true),
            ("a b c", true),
            ("keys", false),
            ("keys nope", false),
            ("keys help", false),
            ("a b", false),
            ("list", false),
            ("keys --url x list", false),
        ] {
            let args = format!("ironflowx {args}");
            let parsed = root.clone().try_get_matches_from(args.split(' '));
            assert_eq!(parsed.is_ok(), ok, "{args}");
        }
    }

    #[test]
    fn the_url_is_the_flags_or_else_the_variables_or_else_the_default() {
        let flag = || Some("f".to_owned());
        let variable = || Some(OsString::from("v"));
        assert_eq!(url(flag(), variable()), ("--url", "f".to_owned()));
        assert_eq!(url(flag(), None), ("--url", "f".to_owned()));
        assert_eq!(url(None, variable()), ("IRONFLOWX_URL", "v".to_owned()));
        let default = "http://127.0.0.1:9310".to_owned();
        assert_eq!(url(None, None), ("the default", default));
    }

    /// D65: a default is added wherever wire JSON leaves one out, and nothing else.
    #[test]
    fn an_answer_is_completed_at_any_depth() {
        const fn field(
            name: &'static str,
            default: Option<&'static str>,
            fields: &'static [Field],
        ) -> Field {
            Field {
                name,
                default,
                fields,
            }
        }
        const ITEM: &[Field] = &[
            field("ok", Some("false"), &[]),
            field("n", Some("\"0\""), &[]),
        ];
        const ANSWER: &[Field] = &[
            field("items", Some("[]"), ITEM),
            field("one", None, &[field("inner", None, ITEM)]),
            field("name", Some("\"\""), &[]),
        ];
        for (sent, printed) in [
            (json!({}), json!({"items": [], "name": ""})),
            (
                json!({"items": [{}, {"ok": true, "x": 1}], "name": "a"}),
                json!({"items": [{"ok": false, "n": "0"}, {"ok": true, "n": "0", "x": 1}], "name": "a"}),
            ),
            (
                json!({"one": {"inner": {"n": "7"}}}),
                json!({"items": [], "name": "", "one": {"inner": {"n": "7", "ok": false}}}),
            ),
            (
                json!({"one": {}}),
                json!({"items": [], "name": "", "one": {}}),
            ),
        ] {
            let mut value = sent.clone();
            complete(&mut value, ANSWER);
            assert_eq!(value, printed, "{sent}");
        }
    }
}
