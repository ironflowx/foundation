//! The record's encoding: binary and length-prefixed. IDs and tokens go in their text forms.
//! A later record is its envelope (format, incarnation token, record ID, attribution, time), then
//! its ordered entries, each with its kind (D112).

use ironflowx_core::{Emitter, EnvRef, IncarnationToken, Timestamp};
use ironflowx_events::{NewEvent, Record};
use uuid::Uuid;

/// What one record commits after its envelope: its time, its events and its execution entries.
/// Events are encoded first; decoding takes the two kinds in any order (D112).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Entries {
    /// The record's time and its events.
    pub(crate) record: Record,
    pub(crate) execution: Vec<ironflowx_execution::Stored>,
}

/// The directory format. A change refuses older directories at open (D8).
pub(crate) const FORMAT: u16 = 3;

const GENESIS: u8 = 0;
const ENTRIES: u8 = 1;

/// An entry's kind: an event. Execution rules number their own kinds from 1 (D112).
const EVENT: u8 = 0;

/// A decoded record.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Decoded {
    /// Position 0: the directory's identity (D14).
    Genesis { format: u16, env: EnvRef },
    Entries {
        format: u16,
        token: IncarnationToken,
        /// Drawn by the writer at the append: a copy of a directory keeps the IDs of the records it
        /// copied, and what each copy appends next has its own (D117).
        id: Uuid,
        emitted_by: Emitter,
        entries: Entries,
    },
}

pub(crate) fn genesis(env: EnvRef) -> Vec<u8> {
    let mut out = vec![GENESIS];
    out.extend_from_slice(&FORMAT.to_le_bytes());
    put(&mut out, env.to_string().as_bytes());
    out
}

pub(crate) fn entries(
    token: IncarnationToken,
    id: Uuid,
    emitted_by: &Emitter,
    entries: &Entries,
) -> Vec<u8> {
    let record = &entries.record;
    let mut out = vec![ENTRIES];
    out.extend_from_slice(&FORMAT.to_le_bytes());
    put(&mut out, token.to_string().as_bytes());
    put(&mut out, id.to_string().as_bytes());
    put(&mut out, emitted_by.to_string().as_bytes());
    out.extend_from_slice(&record.time.as_millis().to_le_bytes());
    let count = record.events.len() + entries.execution.len();
    out.extend_from_slice(&len(count).to_le_bytes());
    for e in &record.events {
        out.push(EVENT);
        out.extend_from_slice(&e.sequence.to_le_bytes());
        put(&mut out, e.id.to_string().as_bytes());
        put(&mut out, e.name.as_bytes());
        out.extend_from_slice(&e.version.to_le_bytes());
        put_opt(&mut out, e.idempotency_key.as_ref().map(String::as_bytes));
        put_opt(&mut out, e.metadata.as_deref());
        put(&mut out, &e.data);
    }
    for entry in &entries.execution {
        ironflowx_execution::encode(entry, &mut out);
    }
    out
}

/// Event rules bound a record to [`MAX_BATCH_EVENTS`](ironflowx_core::limits::MAX_BATCH_EVENTS)
/// events and [`MAX_BATCH_BYTES`](ironflowx_core::limits::MAX_BATCH_BYTES) of data and metadata
/// beside their names and keys, and execution rules bound their entries, so every length fits.
fn len(n: usize) -> u32 {
    u32::try_from(n).expect("event rules bound a record's lengths")
}

fn put(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&len(bytes.len()).to_le_bytes());
    out.extend_from_slice(bytes);
}

fn put_opt(out: &mut Vec<u8>, bytes: Option<&[u8]>) {
    match bytes {
        None => out.push(0),
        Some(b) => {
            out.push(1);
            put(out, b);
        }
    }
}

/// `None` if the bytes are not a record this format writes.
pub(crate) fn decode(bytes: &[u8]) -> Option<Decoded> {
    let mut d = Dec(bytes);
    let kind = d.take::<1>()?[0];
    let format = u16::from_le_bytes(d.take()?);
    let record = match kind {
        GENESIS => Decoded::Genesis {
            format,
            env: d.text()?.parse().ok()?,
        },
        ENTRIES => {
            let token = d.text()?.parse().ok()?;
            let id = record_id(d.text()?)?;
            let emitted_by = d.text()?.parse().ok()?;
            let time = Timestamp::from_millis(i64::from_le_bytes(d.take()?));
            let count = u32::from_le_bytes(d.take()?);
            let (mut events, mut execution) = (Vec::new(), Vec::new());
            for _ in 0..count {
                if d.0.first()? != &EVENT {
                    execution.push(ironflowx_execution::decode(&mut d.0)?);
                    continue;
                }
                d.take::<1>()?;
                events.push(NewEvent {
                    sequence: u64::from_le_bytes(d.take()?),
                    id: d.text()?.parse().ok()?,
                    name: d.text()?.to_owned(),
                    version: i64::from_le_bytes(d.take()?),
                    idempotency_key: d
                        .opt()?
                        .map(|k| String::from_utf8(k.to_vec()))
                        .transpose()
                        .ok()?,
                    metadata: d.opt()?.map(<[u8]>::to_vec),
                    data: d.bytes()?.to_vec(),
                });
            }
            Decoded::Entries {
                format,
                token,
                id,
                emitted_by,
                entries: Entries {
                    record: Record { time, events },
                    execution,
                },
            }
        }
        _ => return None,
    };
    d.0.is_empty().then_some(record)
}

/// A UUIDv7 in its canonical text form, as `ironflowx-core` parses its IDs; the writer draws
/// nothing else.
fn record_id(text: &str) -> Option<Uuid> {
    let id = Uuid::try_parse(text).ok()?;
    let v7 = id.get_version() == Some(uuid::Version::SortRand)
        && id.get_variant() == uuid::Variant::RFC4122;
    (v7 && id.hyphenated().to_string() == text).then_some(id)
}

struct Dec<'a>(&'a [u8]);

impl<'a> Dec<'a> {
    fn take<const N: usize>(&mut self) -> Option<[u8; N]> {
        let (head, rest) = self.0.split_first_chunk::<N>()?;
        self.0 = rest;
        Some(*head)
    }

    fn bytes(&mut self) -> Option<&'a [u8]> {
        let len = u32::from_le_bytes(self.take()?) as usize;
        let (head, rest) = self.0.split_at_checked(len)?;
        self.0 = rest;
        Some(head)
    }

    fn text(&mut self) -> Option<&'a str> {
        std::str::from_utf8(self.bytes()?).ok()
    }

    fn opt(&mut self) -> Option<Option<&'a [u8]>> {
        match self.take::<1>()?[0] {
            0 => Some(None),
            1 => self.bytes().map(Some),
            _ => None,
        }
    }
}
