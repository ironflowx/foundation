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
    answer: String,
    /// The answer's fields that need something before they are printed.
    printed: Vec<Printed>,
}

/// What printing a field of an answer needs.
#[derive(Debug, PartialEq)]
struct Printed {
    name: String,
    /// The JSON text of its default, which the wire leaves out; `None` for a field with presence.
    default: Option<String>,
    /// The fields of the message, or of each of the messages, it holds.
    fields: Vec<Printed>,
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
    let word = |w: &str| !w.is_empty() && w.bytes().all(|b| b.is_ascii_lowercase() || b == b'-');
    if !command.split(' ').all(word) || command.split(' ').any(|w| w.starts_with('-')) {
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
    Ok(Method {
        name: snake_case(method.name()),
        command,
        summary,
        example,
        request: method.input().name().to_owned(),
        input,
        answer: method.output().name().to_owned(),
        printed: printed(&method.output())?,
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

/// A field's JSON Schema, and the default it has when it is not set: none, if it has presence.
fn shape(field: &FieldDescriptor) -> Result<(Value, Option<Value>), CodegenError> {
    let pool = field.parent_message().parent_pool().clone();
    let json_text = option(&pool, &field.options(), "json_text") == Some(OptionValue::Bool(true));
    let (schema, default) = match field.kind() {
        Kind::String if json_text => (
            json!({"type": "string", "contentMediaType": "application/json"}),
            json!(""),
        ),
        _ if json_text => return Err(refused(field.full_name(), "JSON text is a string")),
        Kind::String => (json!({"type": "string"}), json!("")),
        Kind::Bool => (json!({"type": "boolean"}), json!(false)),
        Kind::Int32 | Kind::Sint32 | Kind::Sfixed32 => (
            json!({"type": "integer", "minimum": i32::MIN, "maximum": i32::MAX}),
            json!(0),
        ),
        Kind::Uint32 | Kind::Fixed32 => (
            json!({"type": "integer", "minimum": 0, "maximum": u32::MAX}),
            json!(0),
        ),
        // As protobuf prints them.
        Kind::Int64 | Kind::Sint64 | Kind::Sfixed64 => (
            json!({"type": "string", "pattern": "^-?[0-9]+$"}),
            json!("0"),
        ),
        Kind::Uint64 | Kind::Fixed64 => {
            (json!({"type": "string", "pattern": "^[0-9]+$"}), json!("0"))
        }
        Kind::Message(m) if m.full_name() == "google.protobuf.Timestamp" => (
            json!({"type": "string", "format": "date-time"}),
            Value::Null,
        ),
        Kind::Message(m) if ours(&m) && !m.is_map_entry() => (object(&m)?, Value::Null),
        kind => {
            let why = format!("the generator does not know the type {kind:?}");
            return Err(refused(field.full_name(), why));
        }
    };
    Ok(if field.is_list() {
        (json!({"type": "array", "items": schema}), Some(json!([])))
    } else {
        (schema, (!field.supports_presence()).then_some(default))
    })
}

/// The JSON Schema of `message` in wire JSON, where every field may be left out.
fn object(message: &MessageDescriptor) -> Result<Value, CodegenError> {
    let mut properties = serde_json::Map::new();
    for field in message.fields() {
        properties.insert(field.json_name().to_owned(), shape(&field)?.0);
    }
    Ok(json!({"type": "object", "properties": properties, "additionalProperties": false}))
}

fn printed(message: &MessageDescriptor) -> Result<Vec<Printed>, CodegenError> {
    let mut fields = Vec::new();
    for field in message.fields() {
        let default = shape(&field)?.1.map(|v| v.to_string());
        let inner = match field.kind() {
            Kind::Message(m) if ours(&m) => printed(&m)?,
            _ => Vec::new(),
        };
        if default.is_some() || !inner.is_empty() {
            fields.push(Printed {
                name: field.json_name().to_owned(),
                default,
                fields: inner,
            });
        }
    }
    Ok(fields)
}

/// The code the CLI includes beside its own `Command` and `Field`.
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
            answer,
            printed,
        } = m;
        let fields = fields(printed);
        // What is checked and not yet read goes in as comments.
        writeln!(
            commands,
            "    // Input: {input}\n    // Example: {example}\n    \
             Command {{ path: {command:?}, summary: {summary:?}, answer: {fields} }},"
        )
        .expect("a string takes any text");
        writeln!(
            arms,
            "        {command:?} => {{\n            \
             let request = wire::{request}::default().try_into()?;\n            \
             let answer = wire::{answer}::from(client.{name}(cx, request).await?);\n            \
             serde_json::to_value(answer)\n        }}"
        )
        .expect("a string takes any text");
    }
    format!(
        "// @generated by ironflowx-codegen from the proto.\n\n\
         /// Every method of the proto.\n\
         pub(crate) const COMMANDS: &[Command] = &[\n{commands}];\n\n\
         /// Calls the operation of `command`: its answer, in wire JSON.\n\
         pub(crate) async fn call(command: &str, client: &Client, cx: &Call) \
         -> Result<serde_json::Value, OpError> {{\n    Ok(match command {{\n{arms}        \
         other => unreachable!(\"{{other}} is not in COMMANDS\"),\n    }}\n    \
         .expect(\"a wire message is JSON\"))\n}}\n"
    )
}

fn fields(printed: &[Printed]) -> String {
    let each = printed.iter().map(|p| {
        let Printed { name, default, .. } = p;
        let fields = fields(&p.fields);
        format!("Field {{ name: {name:?}, default: {default:?}, fields: {fields} }}")
    });
    format!("&[{}]", each.collect::<Vec<_>>().join(", "))
}
