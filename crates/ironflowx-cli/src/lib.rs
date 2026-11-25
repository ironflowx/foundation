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

/// How long a key's secret is, the part after its ID.
const SECRET_LEN: usize = 43;

const DEFAULT_URL: &str = "http://127.0.0.1:9310";

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
/// What the process exits with: `--help` has printed its text, or a usage error its envelope; or
/// clap would print the arguments, so none was read.
pub fn parse(root: clap::Command) -> Result<ArgMatches, ExitCode> {
    // A clap that traces its parsing prints each argument before one can be hidden (TEN-3).
    // One that changes this text refuses every start, and the message names the wrong cause.
    if clap::Error::raw(clap::error::ErrorKind::Io, "").to_string() != "error: " {
        return Err(end(Err(invalid("clap is built with its debug feature"))));
    }
    let root = root.subcommand(
        clap::Command::new("version").about("Show the version, the commit and the protocol"),
    );
    // Apart from the hand-written ones, so that clap refuses a name both have.
    let generated = generated::COMMANDS.iter();
    let generated = generated.fold(clap::Command::new("generated"), |all, command| {
        let words: Vec<_> = command.path.split(' ').collect();
        add(all, &words, command)
    });
    let root = root.subcommands(generated.get_subcommands().cloned());
    group(root).try_get_matches().map_err(|e| {
        if !e.use_stderr() {
            return end(e.print().map_err(|e| unprinted(&e)));
        }
        // Its first paragraph; the rest is the usage.
        let text = e.render().to_string();
        let first = text.split("\n\n").next().unwrap_or_default();
        // A key given where none goes, or the end of one, is not printed (TEN-3).
        let key = |word: &str| {
            // Without clap's quotes, one on each side.
            let bare = word.strip_prefix('\'').unwrap_or(word);
            let bare = bare.strip_suffix('\'').unwrap_or(bare);
            word.contains(KEY_PREFIX) || bare.len() >= SECRET_LEN
        };
        let hide = |word| if key(word) { "<hidden>" } else { word };
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
    let (command, leaf) = chosen(matches);
    end(answer(&command, leaf).and_then(|a| {
        print(&a).map_err(|e| match &*command {
            "keys create" => OpError {
                hint: Some(
                    "the key is stored and its secret lost: list the keys, revoke it".into(),
                ),
                ..e
            },
            _ => e,
        })
    }))
}

/// The words of the command that was parsed, and its own arguments.
fn chosen(matches: &ArgMatches) -> (String, &ArgMatches) {
    let mut words = Vec::new();
    let mut leaf = matches;
    while let Some((word, inner)) = leaf.subcommand() {
        words.push(word);
        leaf = inner;
    }
    (words.join(" "), leaf)
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
                .arg(Arg::new("url").value_name("URL").long("url").help(format!(
                    "The engine to call; else IRONFLOWX_URL, else {DEFAULT_URL}"
                )))
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
        .map_err(|e| without_secret(e, &cx))?;
    if repeats_secret(&answer.to_string(), &cx) {
        let message = "the answer is not printed, since it repeats the key the CLI was given";
        return Err(internal(message.to_owned()));
    }
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
        (None, None) => ("the default", DEFAULT_URL.to_owned()),
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

/// Whether `text` holds as many bytes in a row of the key in `cx` as a key's secret has (TEN-3).
/// Fewer are no one's secret, and may be in any answer; a repeat that was changed is not found.
fn repeats_secret(text: &str, cx: &Call) -> bool {
    let key = cx.key().map_or("", Secret::expose).as_bytes();
    // A part that is not text is of a key that is never sent as it is.
    key.windows(SECRET_LEN)
        .filter_map(|part| str::from_utf8(part).ok())
        .any(|part| text.contains(part))
}

/// `e`, whose text may come from no engine, without one that repeats the secret in `cx`.
fn without_secret(mut e: OpError, cx: &Call) -> OpError {
    for text in [Some(&mut e.message), e.hint.as_mut()]
        .into_iter()
        .flatten()
    {
        if repeats_secret(text, cx) {
            *text = "not printed, since it repeats the key the CLI was given".to_owned();
        }
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
    writeln!(out, "{text}").map_err(|e| unprinted(&e))
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
        for (args, found) in [
            ("whoami", Some("whoami")),
            ("keys list --url x --key-file y", Some("keys list")),
            ("keys revoke --url x", Some("keys revoke")),
            ("a b c", Some("a b c")),
            ("keys", None),
            ("keys nope", None),
            ("keys help", None),
            ("a b", None),
            ("list", None),
            ("keys --url x list", None),
        ] {
            let line = format!("ironflowx {args}");
            let parsed = root.clone().try_get_matches_from(line.split(' ')).ok();
            let got = parsed.as_ref().map(chosen);
            assert_eq!(got.as_ref().map(|(c, _)| &**c), found, "{args}");
            let url = got.and_then(|(_, leaf)| leaf.get_one::<String>("url"));
            assert_eq!(
                url.is_some(),
                found.is_some() && args.contains("--url"),
                "{args}"
            );
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
