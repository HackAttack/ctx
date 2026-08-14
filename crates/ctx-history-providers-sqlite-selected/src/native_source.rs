use std::fmt;

#[derive(PartialEq, Eq)]
pub(crate) enum NativeSqliteValue {
    Null,
    Integer(i64),
    Text(String),
}

impl fmt::Debug for NativeSqliteValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Null => formatter.write_str("Null"),
            Self::Integer(_) => formatter.write_str("Integer(<redacted>)"),
            Self::Text(value) => formatter
                .debug_struct("Text")
                .field("bytes", &value.len())
                .finish(),
        }
    }
}
