//! The JSON text rules (D12): one `serde_json` pass with a visitor that decodes every key and
//! string, so `serde_json` unescapes them and refuses unpaired surrogates, and the visitor
//! refuses a repeated key and a NUL. `serde_json` itself refuses numbers out of `f64` range and
//! nesting past 127: its recursion limit is 128, and the tests pin 127 accepted, 128 refused.

use std::collections::BTreeSet;
use std::fmt;

use serde_core::de::{self, DeserializeSeed, Deserializer, MapAccess, SeqAccess, Visitor};

/// Checks one JSON text and says whether its top level is an object. With `forbid_redacted`,
/// a top-level `$redacted` key is refused.
pub(crate) fn check(text: &str, forbid_redacted: bool) -> Result<TopLevel, serde_json::Error> {
    let mut de = serde_json::Deserializer::from_str(text);
    let top = Value { forbid_redacted }.deserialize(&mut de)?;
    de.end()?;
    Ok(top)
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum TopLevel {
    Object,
    Other,
}

#[derive(Clone, Copy)]
struct Value {
    forbid_redacted: bool,
}

/// The seed for a value inside a container: `$redacted` is refused only at the top level.
const INSIDE: Value = Value {
    forbid_redacted: false,
};

fn no_nul<E: de::Error>(s: &str) -> Result<(), E> {
    if s.contains('\0') {
        return Err(E::custom("NUL in a key or string"));
    }
    Ok(())
}

impl<'de> DeserializeSeed<'de> for Value {
    type Value = TopLevel;
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<TopLevel, D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Value {
    type Value = TopLevel;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a JSON value")
    }

    fn visit_bool<E: de::Error>(self, _: bool) -> Result<TopLevel, E> {
        Ok(TopLevel::Other)
    }

    fn visit_i64<E: de::Error>(self, _: i64) -> Result<TopLevel, E> {
        Ok(TopLevel::Other)
    }

    fn visit_u64<E: de::Error>(self, _: u64) -> Result<TopLevel, E> {
        Ok(TopLevel::Other)
    }

    fn visit_f64<E: de::Error>(self, _: f64) -> Result<TopLevel, E> {
        Ok(TopLevel::Other)
    }

    fn visit_unit<E: de::Error>(self) -> Result<TopLevel, E> {
        Ok(TopLevel::Other)
    }

    fn visit_str<E: de::Error>(self, s: &str) -> Result<TopLevel, E> {
        no_nul(s).map(|()| TopLevel::Other)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<TopLevel, A::Error> {
        while seq.next_element_seed(INSIDE)?.is_some() {}
        Ok(TopLevel::Other)
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<TopLevel, A::Error> {
        let mut keys = BTreeSet::new();
        while let Some(key) = map.next_key::<String>()? {
            no_nul(&key)?;
            if self.forbid_redacted && key == "$redacted" {
                return Err(de::Error::custom("top-level $redacted key"));
            }
            if !keys.insert(key) {
                return Err(de::Error::custom("repeated key"));
            }
            map.next_value_seed(INSIDE)?;
        }
        Ok(TopLevel::Object)
    }
}
