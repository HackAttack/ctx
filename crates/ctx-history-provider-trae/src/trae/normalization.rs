use serde_json::Value;

/// Returns the first alias containing a string, then validates that selection.
///
/// Trae historically gives alias order precedence over content. In particular,
/// a blank preferred alias suppresses later populated aliases.
pub(super) fn trae_first_present_string_field(value: &Value, fields: &[&str]) -> Option<String> {
    fields
        .iter()
        .find_map(|field| value.get(*field).and_then(Value::as_str))
        .filter(|text| !text.trim().is_empty())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::trae_first_present_string_field;

    #[test]
    fn blank_preferred_alias_suppresses_later_populated_alias() {
        let value = json!({"preferred": "  ", "later": "must-not-win"});

        assert_eq!(
            trae_first_present_string_field(&value, &["preferred", "later"]),
            None
        );
    }
}
