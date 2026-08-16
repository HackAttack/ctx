use std::fmt;

use serde::de::{self, Visitor};

pub(super) const MAX_CURSOR_ATOM_BYTES: usize = 64 * 1024;

pub(super) struct BoundedStringVisitor {
    pub(super) max_bytes: usize,
}

impl Visitor<'_> for BoundedStringVisitor {
    type Value = String;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an exact bounded string")
    }

    fn visit_borrowed_str<E>(self, value: &str) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        self.exact(value)
    }

    fn visit_str<E>(self, value: &str) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        self.exact(value)
    }

    fn visit_string<E>(self, value: String) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        if value.len() <= self.max_bytes {
            Ok(value)
        } else {
            Err(E::custom("Cursor string exceeds the exact capture limit"))
        }
    }
}

impl BoundedStringVisitor {
    fn exact<E: de::Error>(&self, value: &str) -> std::result::Result<String, E> {
        if value.len() <= self.max_bytes {
            Ok(value.to_owned())
        } else {
            Err(E::custom("Cursor string exceeds the exact capture limit"))
        }
    }
}
