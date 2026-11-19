//! The JSON text rules (D12): one `serde_json` pass with a visitor that decodes every key and
//! string, so `serde_json` unescapes them and refuses unpaired surrogates, and the visitor
//! refuses a repeated key and a NUL. `serde_json` itself refuses numbers out of `f64` range,
//! correctly rounded (`float_roundtrip`), and nesting past 127: its recursion limit is 128, and
//! the tests pin 127 accepted, 128 refused.

use std::collections::BTreeSet;
use std::fmt;

use serde_core::de::{self, DeserializeSeed, Deserializer, MapAccess, SeqAccess, Visitor};

/// With serde_json's `arbitrary_precision` on, which feature unification can do in any build, a
/// number reaches the visitor as a one-entry map under this key, its range unchecked.
// shortcut: in such a build an object whose first key is this one is taken for a number;
// upgrade if a client ever needs that key.
const NUMBER_TOKEN: &str = "$serde_json::private::Number";

/// Checks one JSON text, as event data or metadata.
pub(crate) fn check(text: &str, field: Checked) -> Result<(), serde_json::Error> {
    let mut de = serde_json::Deserializer::from_str(text);
    field.deserialize(&mut de)?;
    de.end()
}

/// The checker for one JSON value: where it sits decides whether `$redacted` is refused, which
/// it is only as a top-level key of data.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Checked {
    Data,
    Metadata,
    Inside,
}

fn no_nul<E: de::Error>(s: &str) -> Result<(), E> {
    if s.contains('\0') {
        return Err(E::custom("NUL in a key or string"));
    }
    Ok(())
}

impl<'de> DeserializeSeed<'de> for Checked {
    type Value = ();
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Checked {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a JSON value")
    }

    fn visit_bool<E: de::Error>(self, _: bool) -> Result<(), E> {
        Ok(())
    }

    fn visit_i64<E: de::Error>(self, _: i64) -> Result<(), E> {
        Ok(())
    }

    fn visit_u64<E: de::Error>(self, _: u64) -> Result<(), E> {
        Ok(())
    }

    fn visit_f64<E: de::Error>(self, _: f64) -> Result<(), E> {
        Ok(())
    }

    fn visit_unit<E: de::Error>(self) -> Result<(), E> {
        Ok(())
    }

    fn visit_str<E: de::Error>(self, s: &str) -> Result<(), E> {
        no_nul(s)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        while seq.next_element_seed(Checked::Inside)?.is_some() {}
        Ok(())
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        let mut keys = BTreeSet::new();
        while let Some(key) = map.next_key::<String>()? {
            if keys.is_empty() && key == NUMBER_TOKEN {
                // Rust's parser rounds correctly, as float_roundtrip does in a default build.
                let number: String = map.next_value()?;
                return match number.parse::<f64>() {
                    Ok(n) if n.is_finite() => Ok(()),
                    _ => Err(de::Error::custom("number out of range")),
                };
            }
            no_nul(&key)?;
            if self == Self::Data && key == "$redacted" {
                return Err(de::Error::custom("top-level $redacted key"));
            }
            if !keys.insert(key) {
                return Err(de::Error::custom("repeated key"));
            }
            map.next_value_seed(Checked::Inside)?;
        }
        Ok(())
    }
}
