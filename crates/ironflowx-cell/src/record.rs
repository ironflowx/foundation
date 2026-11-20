//! The log record: binary and length-prefixed. IDs and tokens go in their text forms.

use ironflowx_core::{EnvRef, IncarnationToken, Timestamp};
use ironflowx_events::{NewEvent, Record};

/// The directory format. A change refuses older directories at open (D8).
pub(crate) const FORMAT: u16 = 1;

const GENESIS: u8 = 0;
const EVENTS: u8 = 1;

/// A decoded log record.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum LogRecord {
    /// Offset 0: the directory's identity (D14).
    Genesis { format: u16, env: EnvRef },
    Events {
        format: u16,
        token: IncarnationToken,
        record: Record,
    },
}

pub(crate) fn genesis(env: EnvRef) -> Vec<u8> {
    let mut out = vec![GENESIS];
    out.extend_from_slice(&FORMAT.to_le_bytes());
    put(&mut out, env.to_string().as_bytes());
    out
}

pub(crate) fn events(token: IncarnationToken, record: &Record) -> Vec<u8> {
    let mut out = vec![EVENTS];
    out.extend_from_slice(&FORMAT.to_le_bytes());
    put(&mut out, token.to_string().as_bytes());
    out.extend_from_slice(&record.time.as_millis().to_le_bytes());
    out.extend_from_slice(&(record.events.len() as u32).to_le_bytes());
    for e in &record.events {
        out.extend_from_slice(&e.sequence.to_le_bytes());
        put(&mut out, e.id.to_string().as_bytes());
        put(&mut out, e.name.as_bytes());
        out.extend_from_slice(&e.version.to_le_bytes());
        put_opt(&mut out, e.idempotency_key.as_ref().map(String::as_bytes));
        put_opt(&mut out, e.metadata.as_deref());
        put(&mut out, &e.data);
    }
    out
}

fn put(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
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
pub(crate) fn decode(bytes: &[u8]) -> Option<LogRecord> {
    let mut d = Dec(bytes);
    let kind = d.take::<1>()?[0];
    let format = u16::from_le_bytes(d.take()?);
    let record = match kind {
        GENESIS => LogRecord::Genesis {
            format,
            env: d.text()?.parse().ok()?,
        },
        EVENTS => {
            let token = d.text()?.parse().ok()?;
            let time = Timestamp::from_millis(i64::from_le_bytes(d.take()?));
            let count = u32::from_le_bytes(d.take()?);
            let mut events = Vec::new();
            for _ in 0..count {
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
            LogRecord::Events {
                format,
                token,
                record: Record { time, events },
            }
        }
        _ => return None,
    };
    d.0.is_empty().then_some(record)
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
