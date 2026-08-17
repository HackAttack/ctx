use super::*;

pub(super) fn json_has_duplicate_key(bytes: &[u8]) -> Result<bool, serde_json::Error> {
    let mut duplicate = false;
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    DuplicateJsonKeySeed(&mut duplicate).deserialize(&mut deserializer)?;
    deserializer.end()?;
    Ok(duplicate)
}

struct DuplicateJsonKeySeed<'a>(&'a mut bool);

impl<'de> DeserializeSeed<'de> for DuplicateJsonKeySeed<'_> {
    type Value = ();

    fn deserialize<D>(self, deserializer: D) -> Result<(), D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(DuplicateJsonKeyVisitor(self.0))
    }
}

struct DuplicateJsonKeyVisitor<'a>(&'a mut bool);

impl<'de> Visitor<'de> for DuplicateJsonKeyVisitor<'_> {
    type Value = ();

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a JSON value")
    }

    fn visit_bool<E>(self, _: bool) -> Result<(), E> {
        Ok(())
    }

    fn visit_i64<E>(self, _: i64) -> Result<(), E> {
        Ok(())
    }

    fn visit_u64<E>(self, _: u64) -> Result<(), E> {
        Ok(())
    }

    fn visit_f64<E>(self, _: f64) -> Result<(), E> {
        Ok(())
    }

    fn visit_str<E>(self, _: &str) -> Result<(), E> {
        Ok(())
    }

    fn visit_string<E>(self, _: String) -> Result<(), E> {
        Ok(())
    }

    fn visit_none<E>(self) -> Result<(), E> {
        Ok(())
    }

    fn visit_unit<E>(self) -> Result<(), E> {
        Ok(())
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<(), A::Error>
    where
        A: SeqAccess<'de>,
    {
        while sequence
            .next_element_seed(DuplicateJsonKeySeed(self.0))?
            .is_some()
        {}
        Ok(())
    }

    fn visit_map<A>(self, mut map: A) -> Result<(), A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut keys = HashSet::new();
        while let Some(key) = map.next_key::<String>()? {
            if !keys.insert(key) {
                *self.0 = true;
            }
            map.next_value_seed(DuplicateJsonKeySeed(self.0))?;
        }
        Ok(())
    }
}

pub(super) fn validate_json_bounds(
    value: &serde_json::Value,
) -> std::result::Result<(), &'static str> {
    let mut stack = vec![(value, 0_usize)];
    let mut collection_elements = 0_usize;
    while let Some((value, depth)) = stack.pop() {
        if depth > SOURCE_BACKED_MAX_JSON_DEPTH {
            return Err("exceeds maximum JSON depth");
        }
        match value {
            serde_json::Value::Array(values) => {
                collection_elements = collection_elements.saturating_add(values.len());
                if collection_elements > SOURCE_BACKED_MAX_COLLECTION_ELEMENTS {
                    return Err("exceeds JSON collection element budget");
                }
                stack.extend(values.iter().map(|value| (value, depth.saturating_add(1))));
            }
            serde_json::Value::Object(values) => {
                collection_elements = collection_elements.saturating_add(values.len());
                if collection_elements > SOURCE_BACKED_MAX_COLLECTION_ELEMENTS {
                    return Err("exceeds JSON collection element budget");
                }
                stack.extend(
                    values
                        .values()
                        .map(|value| (value, depth.saturating_add(1))),
                );
            }
            serde_json::Value::Null
            | serde_json::Value::Bool(_)
            | serde_json::Value::Number(_)
            | serde_json::Value::String(_) => {}
        }
    }
    Ok(())
}

pub(super) fn bounded_failure(error: impl Into<String>) -> String {
    let mut error = error.into();
    if error.len() > SOURCE_BACKED_MAX_FAILURE_BYTES {
        let mut boundary = SOURCE_BACKED_MAX_FAILURE_BYTES;
        while !error.is_char_boundary(boundary) {
            boundary = boundary.saturating_sub(1);
        }
        error.truncate(boundary);
    }
    error
}
