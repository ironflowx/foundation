//! Ironflow X event rules: what an event is and when an emit is refused.
//!
//! [`decide`] checks a submission against the current state and returns what to append;
//! [`engine_event`] makes one of the engine's own events, which [`sequence`] numbers instead;
//! [`fold`] applies an appended record to the read store; [`get_event`] and [`list_events`]
//! read events back. Nothing here does I/O, reads a clock or draws randomness (LED-15): state
//! comes through [`EventsView`] and goes out through [`EventTables`].

// Purity (LED-15): forbid, so an inner #[allow] cannot switch these off.
#![forbid(unsafe_code)]
#![forbid(clippy::disallowed_methods, clippy::disallowed_types)]
#![forbid(clippy::print_stdout, clippy::print_stderr, clippy::dbg_macro)]

mod json;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::num::NonZeroU32;

use ironflowx_core::limits::{
    MAX_BATCH_BYTES, MAX_BATCH_EVENTS, MAX_DATA_BYTES, MAX_EVENT_NAME_BYTES,
    MAX_IDEMPOTENCY_KEY_BYTES, MAX_METADATA_BYTES,
};
use ironflowx_core::{
    Emitter, ErrorKind, EventId, IncarnationToken, RunId, SequencedTime, Timestamp,
};

// Not in `limits`: a page's bytes bound an answer, not what a caller sends.
const MAX_PAGE_BYTES: usize = 3 * 1024 * 1024;

/// One emit as a client sent it. Data and metadata are bytes that must hold JSON (D22).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Emit {
    pub name: String,
    /// `None` stores `null`.
    pub data: Option<Vec<u8>>,
    /// A JSON object, or `None`.
    pub metadata: Option<Vec<u8>>,
    /// `None` stores 1.
    pub version: Option<i64>,
    /// `None`: no key.
    pub idempotency_key: Option<String>,
}

/// What a client asks to commit: one emit, or a batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Submission {
    Emit(Emit),
    Batch(Vec<Emit>),
}

/// A stored event, as the read store holds it and the queries return it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub sequence: u64,
    pub id: EventId,
    pub name: String,
    /// The bytes as sent; the JSON `null` when none were.
    pub data: Vec<u8>,
    pub metadata: Option<Vec<u8>>,
    pub version: i64,
    pub idempotency_key: Option<String>,
    pub time: Timestamp,
    /// The record's attribution, which the ledger stores beside it.
    pub emitted_by: Emitter,
}

/// One new event in a record. Its time is the record's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewEvent {
    pub sequence: u64,
    pub id: EventId,
    pub name: String,
    pub data: Vec<u8>,
    pub metadata: Option<Vec<u8>>,
    pub version: i64,
    pub idempotency_key: Option<String>,
}

/// Everything one submission commits: its event-clock time and its new events, in input order.
/// The ledger adds the envelope (format version, incarnation token) and the encoding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub time: Timestamp,
    pub events: Vec<NewEvent>,
}

/// One item's outcome, in input order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ItemOutcome {
    New(EventId),
    Duplicate(EventId),
    /// Only inside a batch: a refused single emit is an [`Error::Refused`].
    Refused(Refusal),
}

/// What `decide` returns: one outcome per item, and the record to append, if any event is new.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub outcome: Vec<ItemOutcome>,
    pub record: Option<Record>,
}

/// A rule refused the request. Its `Display` shows the message, not the hint; it wraps no error,
/// so it has no `source()` (D84).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    kind: ErrorKind,
    message: String,
    hint: Option<String>,
}

impl Refusal {
    fn invalid(message: impl Into<String>) -> Self {
        Self {
            kind: ErrorKind::Invalid,
            message: message.into(),
            hint: None,
        }
    }

    fn not_found(message: impl Into<String>) -> Self {
        Self {
            kind: ErrorKind::NotFound,
            message: message.into(),
            hint: None,
        }
    }

    pub const fn kind(&self) -> ErrorKind {
        self.kind
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    /// What the caller can do about it, if a rule says.
    pub fn hint(&self) -> Option<&str> {
        self.hint.as_deref()
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Refusal {}

/// A call failed: refused by a rule, or the store failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error<E> {
    Refused(Refusal),
    Store(E),
}

/// The current state, as `decide` and the queries read it.
pub trait EventsView {
    type Error;
    /// The event an idempotency key names, if any.
    fn event_by_key(&self, key: &str) -> Result<Option<EventId>, Self::Error>;
    /// The highest event sequence stored; 0 when none is.
    fn last_sequence(&self) -> Result<u64, Self::Error>;
    fn event(&self, id: EventId) -> Result<Option<Event>, Self::Error>;
    /// The events with a sequence below `before`, or any when `None`, newest first, up to the
    /// first that `budget` refuses. It is asked about each in turn, before that event is loaded;
    /// nothing older than a refused event is looked at.
    fn events_before(
        &self,
        before: Option<u64>,
        budget: &mut PageBudget,
    ) -> Result<Vec<Event>, Self::Error>;
}

/// What a page of events may still hold (D73). Only [`list_events`] makes one, so no read is
/// unbounded.
#[derive(Debug)]
pub struct PageBudget {
    limit: u32,
    count: u32,
    bytes: usize,
    refused: bool,
}

impl PageBudget {
    /// Whether the page holds the next event too, `bytes` of data and metadata together. Once
    /// one is refused, so is every later one.
    pub fn take(&mut self, bytes: usize) -> bool {
        self.bytes = self.bytes.saturating_add(bytes);
        self.refused = self.count == self.limit || (self.count > 0 && self.bytes > MAX_PAGE_BYTES);
        self.count += u32::from(!self.refused);
        !self.refused
    }
}

/// The read store, as `fold` writes it.
pub trait EventTables {
    type Error;
    fn insert(&mut self, event: Event) -> Result<(), Self::Error>;
}

/// Decides a submission against the current state. `now` is the event-clock reading that
/// stamps the record; `fresh` yields one event ID per new event and is drawn only for those.
///
/// # Panics
///
/// If `fresh` runs out.
pub fn decide<V: EventsView>(
    view: &V,
    sub: &Submission,
    now: SequencedTime,
    fresh: &mut impl Iterator<Item = EventId>,
) -> Result<Decision, Error<V::Error>> {
    let (items, batch) = match sub {
        Submission::Emit(emit) => (std::slice::from_ref(emit), false),
        Submission::Batch(items) => (items.as_slice(), true),
    };
    // 1. Batch count.
    if items.len() > MAX_BATCH_EVENTS {
        return Err(Error::Refused(Refusal::invalid(format!(
            "batch size {} exceeds maximum of {MAX_BATCH_EVENTS}",
            items.len()
        ))));
    }
    // 2. Item rules.
    let checked: Vec<_> = items.iter().map(|e| check_emit(e).map(|()| e)).collect();
    if !batch && let [Err(refusal)] = checked.as_slice() {
        return Err(Error::Refused(refusal.clone()));
    }
    // 3. Batch bytes, over the items that passed.
    let bytes: usize = checked
        .iter()
        .flatten()
        .map(|e| e.data.as_ref().map_or(0, Vec::len) + e.metadata.as_ref().map_or(0, Vec::len))
        .sum();
    if bytes > MAX_BATCH_BYTES {
        return Err(Error::Refused(Refusal::invalid(format!(
            "batch data and metadata exceed {} MiB",
            MAX_BATCH_BYTES >> 20
        ))));
    }
    // 4. Every idempotency lookup, in input order, before anything is read or drawn for a new
    // event: `Ok(Ok(emit))` may be new, `Ok(Err(id))` repeats a stored key.
    let mut pending = BTreeSet::new();
    let mut looked = Vec::with_capacity(items.len());
    for item in checked {
        looked.push(match item {
            Ok(emit) => match emit.idempotency_key.as_deref() {
                // Repeats a key earlier in this batch; resolved below.
                Some(key) if pending.contains(key) => Ok(Ok(emit)),
                Some(key) => match view.event_by_key(key).map_err(Error::Store)? {
                    Some(id) => Ok(Err(id)),
                    None => {
                        pending.insert(key);
                        Ok(Ok(emit))
                    }
                },
                None => Ok(Ok(emit)),
            },
            Err(refusal) => Err(refusal),
        });
    }
    // 5. One sequence and one ID per new event; the last sequence is read only if there is one.
    let mut last = None;
    let mut keys = BTreeMap::new();
    let mut outcome = Vec::with_capacity(items.len());
    let mut events = Vec::new();
    for item in looked {
        let emit = match item {
            Ok(Ok(emit)) => emit,
            Ok(Err(id)) => {
                outcome.push(ItemOutcome::Duplicate(id));
                continue;
            }
            Err(refusal) => {
                outcome.push(ItemOutcome::Refused(refusal));
                continue;
            }
        };
        let key = emit.idempotency_key.as_deref();
        if let Some(&id) = key.and_then(|key| keys.get(key)) {
            outcome.push(ItemOutcome::Duplicate(id));
            continue;
        }
        let sequence = match last {
            Some(sequence) => sequence,
            None => view.last_sequence().map_err(Error::Store)?,
        }
        .checked_add(1)
        .ok_or_else(|| Error::Refused(Refusal::invalid("event sequences are used up")))?;
        last = Some(sequence);
        let id = fresh.next().expect("fresh yields an ID per new event");
        if let Some(key) = key {
            keys.insert(key, id);
        }
        outcome.push(ItemOutcome::New(id));
        events.push(NewEvent {
            sequence,
            id,
            name: emit.name.clone(),
            data: emit.data.clone().unwrap_or_else(|| b"null".to_vec()),
            metadata: emit.metadata.clone(),
            version: emit.version.unwrap_or(1),
            idempotency_key: emit.idempotency_key.clone(),
        });
    }
    let record = (!events.is_empty()).then_some(Record {
        time: now.timestamp(),
        events,
    });
    Ok(Decision { outcome, record })
}

// So `check_emit` is all a single emit meets: the batch byte limit cannot refuse one.
const _: () = assert!(MAX_DATA_BYTES + MAX_METADATA_BYTES <= MAX_BATCH_BYTES);
// The refusal names the batch byte limit in whole MiB.
const _: () = assert!(MAX_BATCH_BYTES.is_multiple_of(1 << 20));

/// The item rules: every check [`decide`] makes of one emit, before any lookup (D5). A single
/// emit that passes them is refused only once event sequences are used up.
///
/// # Errors
///
/// The [`Refusal`] [`decide`] would give the emit.
pub fn check_emit(emit: &Emit) -> Result<(), Refusal> {
    check_name(&emit.name)?;
    if let Some(data) = &emit.data {
        if data.len() > MAX_DATA_BYTES {
            return Err(Refusal::invalid("event data exceeds maximum size"));
        }
        json::check(data, json::Checked::Data)
            .map_err(|e| Refusal::invalid(format!("event data is invalid: {e}")))?;
    }
    if let Some(metadata) = &emit.metadata {
        if metadata.len() > MAX_METADATA_BYTES {
            return Err(Refusal::invalid("event metadata exceeds maximum size"));
        }
        json::check(metadata, json::Checked::Metadata)
            .map_err(|e| Refusal::invalid(format!("event metadata is invalid: {e}")))?;
        // Valid JSON, so its first non-whitespace byte says whether it is an object.
        if !metadata.trim_ascii_start().starts_with(b"{") {
            return Err(Refusal::invalid("event metadata is not a JSON object"));
        }
    }
    if emit
        .version
        .is_some_and(|v| !(1..=i64::from(i32::MAX)).contains(&v))
    {
        return Err(Refusal::invalid("event version is outside 1 to 2147483647"));
    }
    if let Some(key) = &emit.idempotency_key
        && (key.is_empty() || key.len() > MAX_IDEMPOTENCY_KEY_BYTES || key.contains('\0'))
    {
        return Err(Refusal::invalid(format!(
            "idempotency key must be 1 to {MAX_IDEMPOTENCY_KEY_BYTES} bytes with no NUL"
        )));
    }
    Ok(())
}

fn check_name(name: &str) -> Result<(), Refusal> {
    if name.is_empty() {
        return Err(Refusal::invalid("event name is required"));
    }
    if name.len() > MAX_EVENT_NAME_BYTES {
        return Err(Refusal::invalid(format!(
            "event name exceeds {MAX_EVENT_NAME_BYTES} bytes"
        )));
    }
    if name.split('.').any(str::is_empty) {
        return Err(Refusal::invalid("event name has an empty segment"));
    }
    if name.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(Refusal::invalid(
            "event name contains whitespace or a control character",
        ));
    }
    let owned =
        |root: &str| name == root || name.strip_prefix(root).is_some_and(|r| r.starts_with('.'));
    if owned("ironflowx") || owned("$system") {
        return Err(Refusal::invalid("event name is owned by the engine"));
    }
    Ok(())
}

/// One of the engine's own events (D118). Its name is owned, so [`decide`] refuses it from
/// everyone; only [`engine_event`] turns one into what [`sequence`] takes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineEvent {
    /// `ironflowx.run.completed`.
    RunCompleted {
        run_id: RunId,
        function: String,
        code: String,
    },
    /// `ironflowx.run.failed`, with the run's error.
    RunFailed {
        run_id: RunId,
        function: String,
        code: String,
        error_code: String,
        error_message: String,
    },
}

/// An engine event as [`engine_event`] made it, for [`sequence`]. Nothing else makes one: no
/// client value converts into it.
///
/// ```compile_fail,E0308
/// let _: ironflowx_events::EngineEmit = ironflowx_events::Emit::default();
/// ```
///
/// ```compile_fail,E0277
/// let _: ironflowx_events::EngineEmit = ironflowx_events::Emit::default().into();
/// ```
///
/// ```compile_fail,E0451
/// let _ = ironflowx_events::EngineEmit { name: "ironflowx.run.completed", data: Vec::new() };
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineEmit {
    name: &'static str,
    data: Vec<u8>,
}

/// Makes an engine event: version 1, no metadata, no idempotency key, and data
/// `{"run_id","function","code"}`, plus `"error":{"code","message"}` for a failure. Only
/// `ironflowx-execution` calls it (RUN-14; a clippy ban in every other crate).
pub fn engine_event(event: EngineEvent) -> EngineEmit {
    let text = |s: &str| serde_json::Value::from(s).to_string();
    let (name, run_id, function, code, error) = match &event {
        EngineEvent::RunCompleted {
            run_id,
            function,
            code,
        } => ("ironflowx.run.completed", run_id, function, code, None),
        EngineEvent::RunFailed {
            run_id,
            function,
            code,
            error_code,
            error_message,
        } => (
            "ironflowx.run.failed",
            run_id,
            function,
            code,
            Some((error_code, error_message)),
        ),
    };
    let mut data = format!(
        r#"{{"run_id":"{run_id}","function":{},"code":{}"#,
        text(function),
        text(code)
    );
    if let Some((code, message)) = error {
        data += &format!(
            r#","error":{{"code":{},"message":{}}}"#,
            text(code),
            text(message)
        );
    }
    data.push('}');
    EngineEmit {
        name,
        data: data.into_bytes(),
    }
}

/// Sequences engine events after every stored event, in order, one ID from `fresh` each: the
/// sequences a client's events take, never refused for their names (D118). Neither the last
/// sequence nor `fresh` is read for none.
///
/// # Panics
///
/// If `fresh` runs out.
pub fn sequence<V: EventsView>(
    view: &V,
    emits: Vec<EngineEmit>,
    fresh: &mut impl Iterator<Item = EventId>,
) -> Result<Vec<NewEvent>, Error<V::Error>> {
    if emits.is_empty() {
        return Ok(Vec::new());
    }
    let last = view.last_sequence().map_err(Error::Store)?;
    (1..)
        .zip(emits)
        .map(|(n, emit)| {
            let sequence = last
                .checked_add(n)
                .ok_or_else(|| Error::Refused(Refusal::invalid("event sequences are used up")))?;
            Ok(NewEvent {
                sequence,
                id: fresh.next().expect("fresh yields an ID per engine event"),
                name: emit.name.to_owned(),
                data: emit.data,
                metadata: None,
                version: 1,
                idempotency_key: None,
            })
        })
        .collect()
}

/// Applies one appended record to the read store. Every event carries the record's one
/// emitter.
pub fn fold<T: EventTables>(
    tables: &mut T,
    record: &Record,
    emitted_by: &Emitter,
) -> Result<(), T::Error> {
    for e in &record.events {
        tables.insert(Event {
            sequence: e.sequence,
            id: e.id,
            name: e.name.clone(),
            data: e.data.clone(),
            metadata: e.metadata.clone(),
            version: e.version,
            idempotency_key: e.idempotency_key.clone(),
            time: record.time,
            emitted_by: emitted_by.clone(),
        })?;
    }
    Ok(())
}

/// One page of events, newest first. `next` continues after the last one; `None` on the last
/// page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventPage {
    pub events: Vec<Event>,
    pub next: Option<String>,
}

/// Lists events newest first by event sequence. `limit` is 20 when `None` and at most 200. A
/// page, and the read behind it, stops before its data and metadata pass 3 MiB, but holds at
/// least one event if any is left (D73). A cursor that does not decode, or names an incarnation
/// other than `token`, is `Invalid`.
pub fn list_events<V: EventsView>(
    view: &V,
    token: IncarnationToken,
    limit: Option<NonZeroU32>,
    cursor: Option<&str>,
) -> Result<EventPage, Error<V::Error>> {
    let before = cursor
        .map(|cursor| {
            decode_cursor(cursor, token)
                .ok_or_else(|| Error::Refused(Refusal::invalid("invalid cursor")))
        })
        .transpose()?;
    let mut budget = PageBudget {
        limit: limit.map_or(20, |l| l.get().min(200)),
        count: 0,
        bytes: 0,
        refused: false,
    };
    let events = view
        .events_before(before, &mut budget)
        .map_err(Error::Store)?;
    // Refused: an event is left. So a full last page carries no cursor.
    let last = events.last().filter(|_| budget.refused);
    let next = last.map(|e| format!("{token}.{}", e.sequence));
    Ok(EventPage { events, next })
}

/// A cursor is `<incarnation token>.<last event sequence returned>`: an encoding, not a
/// signature.
fn decode_cursor(cursor: &str, token: IncarnationToken) -> Option<u64> {
    let (t, sequence) = cursor.split_once('.')?;
    let sequence: u64 = sequence.parse().ok()?;
    // Only the form this module writes: no sign, no leading zeros.
    (t.parse::<IncarnationToken>().ok()? == token && cursor == format!("{token}.{sequence}"))
        .then_some(sequence)
}

/// Reads one event by ID. An unknown ID, or one that does not parse, is `NotFound`.
pub fn get_event<V: EventsView>(view: &V, id: &str) -> Result<Event, Error<V::Error>> {
    let not_found = || Error::Refused(Refusal::not_found("event not found"));
    let id = id.parse().map_err(|_| not_found())?;
    view.event(id).map_err(Error::Store)?.ok_or_else(not_found)
}
