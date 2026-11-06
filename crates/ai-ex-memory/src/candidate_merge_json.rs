//! Validate JSON without discarding duplicate keys in unknown fields.

use std::collections::BTreeSet;
use std::fmt;

use serde::de::{DeserializeSeed, Error, MapAccess, SeqAccess, Visitor};

const MAX_DEPTH: usize = 64;
const MAX_ITEMS: usize = 50_000;

pub(super) fn validate(line: &str) -> Result<(), ()> {
    let mut input = serde_json::Deserializer::from_str(line);
    Unique { depth: 0 }
        .deserialize(&mut input)
        .map_err(|_| ())?;
    input.end().map_err(|_| ())
}

struct Unique {
    depth: usize,
}

impl<'de> DeserializeSeed<'de> for Unique {
    type Value = ();

    fn deserialize<D: serde::Deserializer<'de>>(self, input: D) -> Result<(), D::Error> {
        input.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Unique {
    type Value = ();

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("bounded JSON without duplicate object keys")
    }

    fn visit_bool<E: Error>(self, _: bool) -> Result<(), E> {
        Ok(())
    }

    fn visit_i64<E: Error>(self, _: i64) -> Result<(), E> {
        Ok(())
    }

    fn visit_u64<E: Error>(self, _: u64) -> Result<(), E> {
        Ok(())
    }

    fn visit_f64<E: Error>(self, _: f64) -> Result<(), E> {
        Ok(())
    }

    fn visit_str<E: Error>(self, _: &str) -> Result<(), E> {
        Ok(())
    }

    fn visit_unit<E: Error>(self) -> Result<(), E> {
        Ok(())
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut values: A) -> Result<(), A::Error> {
        if self.depth >= MAX_DEPTH {
            return Err(A::Error::custom("JSON nesting exceeds limit"));
        }
        let mut count = 0;
        while values
            .next_element_seed(Unique {
                depth: self.depth + 1,
            })?
            .is_some()
        {
            count += 1;
            if count > MAX_ITEMS {
                return Err(A::Error::custom("JSON array exceeds limit"));
            }
        }
        Ok(())
    }

    fn visit_map<A: MapAccess<'de>>(self, mut values: A) -> Result<(), A::Error> {
        if self.depth >= MAX_DEPTH {
            return Err(A::Error::custom("JSON nesting exceeds limit"));
        }
        let mut keys = BTreeSet::new();
        while let Some(key) = values.next_key::<String>()? {
            if keys.len() >= MAX_ITEMS || !keys.insert(key) {
                return Err(A::Error::custom("duplicate or excessive JSON object keys"));
            }
            values.next_value_seed(Unique {
                depth: self.depth + 1,
            })?;
        }
        Ok(())
    }
}
