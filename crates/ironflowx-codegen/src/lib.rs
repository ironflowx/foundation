//! Generates the CLI's commands from the proto's descriptor set, at build time (D52).

#![forbid(unsafe_code)]

use std::fmt::{self, Write as _};
use std::path::Path;

use prost_reflect::{
    DescriptorPool, DynamicMessage, FieldDescriptor, Kind, MessageDescriptor, MethodDescriptor,
    Value as OptionValue,
};
use serde_json::{Map, Value, json};

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
    rpc: String,
    summary: String,
    example: Value,
    request: String,
    /// The JSON Schema of the wire request.
    input: Value,
    /// The JSON Schema of the answer as the CLI prints it.
    output: Value,
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
    let input = schema(&method.input(), Shape::Input)?;
    let validator = jsonschema::options()
        .with_draft(jsonschema::Draft::Draft202012)
        .should_validate_formats(true)
        .build(&input)
        .expect("a schema this generates is one");
    if let Err(e) = validator.validate(&example) {
        let why = format!("its (ironflowx.v1.example) fails its schema: {e}");
        return Err(refused(at, why));
    }
    // The decoder also checks the request message, including oneof cardinality.
    DynamicMessage::deserialize(method.input(), &example)
        .map_err(|e| refused(at, format!("its (ironflowx.v1.example) is no request: {e}")))?;
    Ok(Method {
        name: snake_case(method.name()),
        command,
        rpc: format!("{}/{}", method.parent_service().full_name(), method.name()),
        summary,
        example,
        request: method.input().name().to_owned(),
        input,
        output: schema(&method.output(), Shape::Output)?,
        asked: fields(&method.input()),
        answer: method.output().name().to_owned(),
        printed: fields(&method.output()),
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum Shape {
    Input,
    Output,
}

fn schema(message: &MessageDescriptor, shape: Shape) -> Result<Value, CodegenError> {
    let mut root = object(message, shape)?;
    root["$schema"] = json!("https://json-schema.org/draft/2020-12/schema");
    Ok(root)
}

/// The digits of a 64-bit integer's magnitude, up to and including its bound.
fn decimal_magnitude(bound: u64) -> String {
    let bound = bound.to_string();
    let mut pattern = format!("[0-9]{{1,{}}}", bound.len() - 1);
    for (i, digit) in bound.bytes().enumerate() {
        if digit > b'0' {
            let prefix = &bound[..i];
            let end = char::from(digit - 1);
            let remaining = bound.len() - i - 1;
            write!(pattern, "|{prefix}[0-{end}][0-9]{{{remaining}}}")
                .expect("a string takes any text");
        }
    }
    format!("{pattern}|{bound}")
}

/// A field's JSON Schema, or that of each item for a repeated field.
fn property(field: &FieldDescriptor, shape: Shape) -> Result<Value, CodegenError> {
    let schema = match field.kind() {
        Kind::String if json_text(field) => match shape {
            Shape::Input => json!({"contentMediaType": "application/json", "type": "string"}),
            Shape::Output => json!(true),
        },
        _ if json_text(field) => return Err(refused(field.full_name(), "JSON text is a string")),
        Kind::String => json!({"type": "string"}),
        Kind::Bool => json!({"type": "boolean"}),
        Kind::Int32 | Kind::Sint32 | Kind::Sfixed32 => {
            json!({"maximum": i32::MAX, "minimum": i32::MIN, "type": "integer"})
        }
        Kind::Uint32 | Kind::Fixed32 => {
            json!({"maximum": u32::MAX, "minimum": 0, "type": "integer"})
        }
        // Quoted decimals, as protobuf prints them. The decoder accepts leading zeros.
        Kind::Int64 | Kind::Sint64 | Kind::Sfixed64 => {
            let max = decimal_magnitude(i64::MAX.unsigned_abs());
            let min = decimal_magnitude(i64::MIN.unsigned_abs());
            json!({"pattern": format!("^(?:0*(?:{max})|-0*(?:{min}))$"), "type": "string"})
        }
        Kind::Uint64 | Kind::Fixed64 => {
            let max = decimal_magnitude(u64::MAX);
            json!({"pattern": format!("^0*(?:{max})$"), "type": "string"})
        }
        Kind::Message(m) if m.full_name() == "google.protobuf.Timestamp" => {
            // Whole milliseconds with up to nine fractional digits, as the wire takes them.
            json!({
                "format": "date-time",
                "not": {"pattern": "^0000-"},
                "pattern": concat!(
                    r"^[0-9]{4}-[0-9]{2}-[0-9]{2}T(?:[01][0-9]|2[0-3]):[0-5][0-9]:[0-5][0-9]",
                    r"(?:\.[0-9]{1,3}0{0,6})?(?:Z|[+-](?:[01][0-9]|2[0-3]):[0-5][0-9])$",
                ),
                "type": "string",
            })
        }
        Kind::Message(m) if ours(&m) && !m.is_map_entry() => object(&m, shape)?,
        kind => {
            let why = format!("the generator does not know the type {kind:?}");
            return Err(refused(field.full_name(), why));
        }
    };
    let schema = domain_property(field, schema);
    Ok(if field.is_list() {
        json!({"items": schema, "type": "array"})
    } else {
        schema
    })
}

/// Restrictions of the domain types that the wire mapping reads and prints.
fn domain_property(field: &FieldDescriptor, mut schema: Value) -> Value {
    match field.full_name() {
        "ironflowx.v1.ListEventsRequest.limit" | "ironflowx.v1.ListKeysRequest.limit" => {
            schema["minimum"] = json!(1);
        }
        "ironflowx.v1.CreateKeyRequest.scopes"
        | "ironflowx.v1.KeyInfo.scopes"
        | "ironflowx.v1.WhoamiResponse.scopes" => {
            schema["enum"] = json!(["read", "write", "admin", "execute"]);
        }
        "ironflowx.v1.RevokeKeyRequest.key_id"
        | "ironflowx.v1.WhoamiResponse.key_id"
        | "ironflowx.v1.KeyInfo.key_id"
        | "ironflowx.v1.KeyInfo.created_by"
        | "ironflowx.v1.GetEventRequest.event_id"
        | "ironflowx.v1.Event.event_id"
        | "ironflowx.v1.EmitResponse.event_id"
        | "ironflowx.v1.Environment.project_id"
        | "ironflowx.v1.GetRunRequest.run_id"
        | "ironflowx.v1.StartRunResponse.run_id"
        | "ironflowx.v1.Run.run_id"
        | "ironflowx.v1.RunEnd.run_id"
        | "ironflowx.v1.Claim.run_id" => {
            schema["pattern"] =
                json!("^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$");
        }
        "ironflowx.v1.Run.status" | "ironflowx.v1.RunEnd.status" => {
            schema["enum"] = json!(["queued", "running", "completed", "failed"]);
        }
        _ => {}
    }
    schema
}

/// Each object is closed. Requests can omit fields at the protobuf layer.
// shortcut: a message that holds itself overflows the stack, so the build fails without naming it;
// refuse it by name when a request or an answer needs one.
fn object(message: &MessageDescriptor, shape: Shape) -> Result<Value, CodegenError> {
    let mut properties = Map::new();
    let mut required = Vec::new();
    for field in message.fields() {
        let name = field.json_name();
        properties.insert(name.to_owned(), property(&field, shape)?);
        if shape == Shape::Output && always_printed(&field) {
            required.push(name.to_owned());
        }
    }
    let mut schema =
        json!({"additionalProperties": false, "properties": properties, "type": "object"});
    if !required.is_empty() {
        schema["required"] = json!(required);
    }
    // An answer of several shapes holds exactly one of them. `optional` makes a oneof of its own,
    // a synthetic one, which says nothing of the kind.
    if shape == Shape::Output {
        for oneof in message.oneofs().filter(|o| !o.is_synthetic()) {
            let shapes: Vec<_> = oneof
                .fields()
                .map(|f| json!({"required": [f.json_name()]}))
                .collect();
            schema["oneOf"] = json!(shapes);
        }
    }
    Ok(schema)
}

/// Defaults are completed by the CLI. These fields with presence are set by wire conversion.
fn always_printed(field: &FieldDescriptor) -> bool {
    !field.supports_presence()
        || matches!(
            field.full_name(),
            "ironflowx.v1.CreateKeyResponse.info"
                | "ironflowx.v1.Environment.created_at"
                | "ironflowx.v1.Event.data"
                | "ironflowx.v1.Event.time"
                | "ironflowx.v1.KeyInfo.created_at"
                | "ironflowx.v1.PutFunctionResponse.function"
                | "ironflowx.v1.Function.put_at"
                | "ironflowx.v1.Run.created_at"
                | "ironflowx.v1.Claim.lease_until"
                | "ironflowx.v1.StepCompleted.output"
                | "ironflowx.v1.Ended.run"
                | "ironflowx.v1.CompleteRunResponse.ended"
        )
}

/// The JSON default that the CLI adds when wire JSON omits it.
fn default(field: &FieldDescriptor) -> Option<&'static str> {
    if field.is_list() {
        return Some("[]");
    }
    if field.supports_presence() {
        return None;
    }
    Some(match field.kind() {
        Kind::String => r#""""#,
        Kind::Bool => "false",
        Kind::Int64 | Kind::Sint64 | Kind::Sfixed64 | Kind::Uint64 | Kind::Fixed64 => r#""0""#,
        _ => "0",
    })
}

/// The message's fields, as code: the CLI's `Field`s.
fn fields(message: &MessageDescriptor) -> String {
    let mut code = Vec::new();
    for field in message.fields() {
        let default = default(&field);
        let kind = match field.kind() {
            Kind::Message(m) if ours(&m) => format!("Message({})", fields(&m)),
            Kind::Bool => "Bool".to_owned(),
            _ if json_text(&field) => "JsonText".to_owned(),
            _ => "Text".to_owned(),
        };
        let (name, list) = (field.json_name(), field.is_list());
        code.push(format!(
            "Field {{ name: {name:?}, kind: Kind::{kind}, list: {list}, default: {default:?} }}"
        ));
    }
    format!("&[{}]", code.join(", "))
}

/// The code the CLI includes beside its own `Command`, `Field` and `Kind`.
fn render(methods: &[Method]) -> String {
    let mut commands = String::new();
    let mut arms = String::new();
    for m in methods {
        let Method {
            name,
            command,
            rpc,
            summary,
            example,
            request,
            input,
            output,
            asked,
            answer,
            printed,
        } = m;
        let (input, output, example) = (input.to_string(), output.to_string(), example.to_string());
        writeln!(
            commands,
            "    Command {{ path: {command:?}, summary: {summary:?}, rpc: {rpc:?}, \
             input: {input:?}, output: {output:?}, example: {example:?}, request: {asked}, \
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
