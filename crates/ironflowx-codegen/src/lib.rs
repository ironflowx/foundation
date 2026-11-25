//! Generates the CLI's commands from the proto's descriptor set, at build time (D52).

#![forbid(unsafe_code)]

use std::fmt::{self, Write as _};
use std::path::Path;

use prost_reflect::{
    DescriptorPool, DynamicMessage, FieldDescriptor, Kind, MessageDescriptor, MethodDescriptor,
    Value as OptionValue,
};
use serde_json::{Value, json};

/// Why a descriptor set generates nothing: the method or field, and what is wrong with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodegenError {
    at: String,
    why: String,
}

impl fmt::Display for CodegenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.at, self.why)
    }
}

impl std::error::Error for CodegenError {}

fn refused(at: &str, why: impl fmt::Display) -> CodegenError {
    CodegenError {
        at: at.to_owned(),
        why: why.to_string(),
    }
}

/// Writes `cli.rs` to `out_dir`: a command per method of `descriptor_set`, an encoded
/// `FileDescriptorSet` with its custom options.
///
/// # Errors
///
/// [`CodegenError`] for a method without a `cli`, a `summary` or an `example`, or with a command
/// that clashes with another's; an example its request's schema refuses; a field whose `json_name`
/// is not its snake_case name (D78); a type the generator does not know; a set that does not
/// decode; and `cli.rs` not written.
pub fn generate(descriptor_set: &[u8], out_dir: &Path) -> Result<(), CodegenError> {
    let code = render(&read(descriptor_set)?);
    let path = out_dir.join("cli.rs");
    std::fs::write(&path, code).map_err(|e| refused(&path.to_string_lossy(), e))
}

/// A method, as its options and its messages describe it.
#[derive(Debug, PartialEq)]
struct Method {
    /// As the `Operations` method is named.
    name: String,
    command: String,
    summary: String,
    example: Value,
    request: String,
    /// The JSON Schema of the wire request.
    input: Value,
    /// The request's fields, as the CLI's `Field`s.
    asked: String,
    answer: String,
    /// The answer's, likewise.
    printed: String,
}

fn read(descriptor_set: &[u8]) -> Result<Vec<Method>, CodegenError> {
    let pool = DescriptorPool::decode(descriptor_set).map_err(|e| refused("descriptor set", e))?;
    let messages = pool.all_messages().filter(ours);
    for field in messages.flat_map(|m| m.fields().collect::<Vec<_>>()) {
        check_name(&field)?;
    }
    let methods = pool
        .services()
        .flat_map(|s| s.methods().collect::<Vec<_>>())
        .map(|m| read_method(&m))
        .collect::<Result<Vec<_>, _>>()?;
    for (i, m) in methods.iter().enumerate() {
        // Nor the group another is in: `events`, beside `events emit`.
        let taken = |other: &&Method| {
            let (a, b) = (&m.command, &other.command);
            a == b || a.starts_with(&format!("{b} ")) || b.starts_with(&format!("{a} "))
        };
        if let Some(other) = methods[..i].iter().find(taken) {
            let why = format!("its command clashes with that of {}", other.name);
            return Err(refused(&m.name, why));
        }
    }
    Ok(methods)
}

fn ours(message: &MessageDescriptor) -> bool {
    message.package_name() != "google.protobuf"
}

/// One spelling on the wire, in `--json` and in output (D78).
fn check_name(field: &FieldDescriptor) -> Result<(), CodegenError> {
    let name = field.name();
    let snake = name
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    if snake && field.json_name() == name {
        return Ok(());
    }
    let why = format!("its json_name, {}, is not snake_case", field.json_name());
    Err(refused(field.full_name(), why))
}

/// The custom option `name` of a method or a field; its default, if it is not set.
fn option(pool: &DescriptorPool, options: &DynamicMessage, name: &str) -> Option<OptionValue> {
    let extension = pool.get_extension_by_name(&format!("ironflowx.v1.{name}"))?;
    Some(options.get_extension(&extension).into_owned())
}

fn read_method(method: &MethodDescriptor) -> Result<Method, CodegenError> {
    let at = method.full_name();
    let text = |name: &str| {
        let set = option(method.parent_pool(), &method.options(), name);
        let text = set.and_then(|v| v.as_str().map(str::to_owned));
        text.filter(|t| !t.trim().is_empty())
            .ok_or_else(|| refused(at, format!("has no (ironflowx.v1.{name})")))
    };
    let command = text("cli")?;
    let part = |p: &str| !p.is_empty() && p.bytes().all(|b| b.is_ascii_lowercase());
    if !command.split([' ', '-']).all(part) {
        return Err(refused(
            at,
            "its (ironflowx.v1.cli) is not kebab-case words",
        ));
    }
    let summary = text("summary")?;
    if summary.contains(char::is_control) {
        return Err(refused(at, "its (ironflowx.v1.summary) is not one line"));
    }
    let example = text("example")?;
    let example: Value = serde_json::from_str(&example)
        .map_err(|e| refused(at, format!("its (ironflowx.v1.example) is not JSON: {e}")))?;
    let input = object(&method.input())?;
    let schema = jsonschema::options()
        .with_draft(jsonschema::Draft::Draft202012)
        .should_validate_formats(true)
        .build(&input)
        .expect("a schema this generates is one");
    if let Err(e) = schema.validate(&example) {
        let why = format!("its (ironflowx.v1.example) fails its schema: {e}");
        return Err(refused(at, why));
    }
    // What a schema does not say: a 64-bit number's bounds, a time's years, one of a oneof.
    DynamicMessage::deserialize(method.input(), &example)
        .map_err(|e| refused(at, format!("its (ironflowx.v1.example) is no request: {e}")))?;
    Ok(Method {
        name: snake_case(method.name()),
        command,
        summary,
        example,
        request: method.input().name().to_owned(),
        input,
        asked: fields(&method.input())?,
        answer: method.output().name().to_owned(),
        printed: fields(&method.output())?,
    })
}

fn snake_case(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_uppercase() && !out.is_empty() {
            out.push('_');
        }
        out.push(c.to_ascii_lowercase());
    }
    out
}

fn json_text(field: &FieldDescriptor) -> bool {
    let pool = field.parent_message().parent_pool().clone();
    option(&pool, &field.options(), "json_text") == Some(OptionValue::Bool(true))
}

/// A field's JSON Schema, and the default it has when it is not set: none, if it has presence.
fn shape(field: &FieldDescriptor) -> Result<(Value, Option<Value>), CodegenError> {
    let json_text = json_text(field);
    let (schema, default) = match field.kind() {
        Kind::String if json_text => (
            json!({"contentMediaType": "application/json", "type": "string"}),
            json!(""),
        ),
        _ if json_text => return Err(refused(field.full_name(), "JSON text is a string")),
        Kind::String => (json!({"type": "string"}), json!("")),
        Kind::Bool => (json!({"type": "boolean"}), json!(false)),
        Kind::Int32 | Kind::Sint32 | Kind::Sfixed32 => (
            json!({"maximum": i32::MAX, "minimum": i32::MIN, "type": "integer"}),
            json!(0),
        ),
        Kind::Uint32 | Kind::Fixed32 => (
            json!({"maximum": u32::MAX, "minimum": 0, "type": "integer"}),
            json!(0),
        ),
        // As protobuf prints them.
        Kind::Int64 | Kind::Sint64 | Kind::Sfixed64 => (
            json!({"pattern": "^-?[0-9]+$", "type": "string"}),
            json!("0"),
        ),
        Kind::Uint64 | Kind::Fixed64 => {
            (json!({"pattern": "^[0-9]+$", "type": "string"}), json!("0"))
        }
        Kind::Message(m) if m.full_name() == "google.protobuf.Timestamp" => (
            // In whole milliseconds, as the wire takes one.
            json!({
                "format": "date-time",
                "pattern": r"^[^.]*(\.[0-9]{1,3}0*)?[^.0-9][^.]*$",
                "type": "string",
            }),
            Value::Null,
        ),
        Kind::Message(m) if ours(&m) && !m.is_map_entry() => (object(&m)?, Value::Null),
        kind => {
            let why = format!("the generator does not know the type {kind:?}");
            return Err(refused(field.full_name(), why));
        }
    };
    Ok(if field.is_list() {
        (json!({"items": schema, "type": "array"}), Some(json!([])))
    } else {
        (schema, (!field.supports_presence()).then_some(default))
    })
}

/// The JSON Schema of `message` in wire JSON, where every field may be left out.
// shortcut: a message that holds itself overflows the stack, so the build fails without naming it;
// refuse it by name when a request or an answer needs one.
fn object(message: &MessageDescriptor) -> Result<Value, CodegenError> {
    let mut properties = serde_json::Map::new();
    for field in message.fields() {
        properties.insert(field.json_name().to_owned(), shape(&field)?.0);
    }
    Ok(json!({"additionalProperties": false, "properties": properties, "type": "object"}))
}

/// The message's fields, as code: the CLI's `Field`s.
fn fields(message: &MessageDescriptor) -> Result<String, CodegenError> {
    let mut code = Vec::new();
    for field in message.fields() {
        let default = shape(&field)?.1.map(|v| v.to_string());
        let kind = match field.kind() {
            Kind::Message(m) if ours(&m) => format!("Message({})", fields(&m)?),
            Kind::Bool => "Bool".to_owned(),
            _ if json_text(&field) => "JsonText".to_owned(),
            _ => "Text".to_owned(),
        };
        let (name, list) = (field.json_name(), field.is_list());
        code.push(format!(
            "Field {{ name: {name:?}, kind: Kind::{kind}, list: {list}, default: {default:?} }}"
        ));
    }
    Ok(format!("&[{}]", code.join(", ")))
}

/// The code the CLI includes beside its own `Command`, `Field` and `Kind`.
fn render(methods: &[Method]) -> String {
    let mut commands = String::new();
    let mut arms = String::new();
    for m in methods {
        let Method {
            name,
            command,
            summary,
            example,
            request,
            input,
            asked,
            answer,
            printed,
        } = m;
        // What is checked and not yet read goes in as comments.
        writeln!(
            commands,
            "    // Input: {input}\n    // Example: {example}\n    \
             Command {{ path: {command:?}, summary: {summary:?}, request: {asked}, \
             answer: {printed}, \
             refusal: |v| serde_json::from_value::<wire::{request}>(v).err() }},"
        )
        .expect("a string takes any text");
        writeln!(
            arms,
            "        {command:?} => {{\n            \
             let request: wire::{request} = \
             serde_json::from_str(request).map_err(|e| undecoded(&e))?;\n            \
             let request = request.try_into()?;\n            \
             let answer = wire::{answer}::from(client.{name}(cx, request).await?);\n            \
             serde_json::to_value(answer)\n        }}"
        )
        .expect("a string takes any text");
    }
    format!(
        "// @generated by ironflowx-codegen from the proto.\n\n\
         /// Every method of the proto.\n\
         pub(crate) const COMMANDS: &[Command] = &[\n{commands}];\n\n\
         /// Calls the operation of `command` with `request`, in wire JSON: its answer, likewise.\n\
         pub(crate) async fn call(command: &str, client: &Client, cx: &Call, request: &str) \
         -> Result<serde_json::Value, OpError> {{\n    Ok(match command {{\n{arms}        \
         other => unreachable!(\"{{other}} is not in COMMANDS\"),\n    }}\n    \
         .expect(\"a wire message is JSON\"))\n}}\n"
    )
}
