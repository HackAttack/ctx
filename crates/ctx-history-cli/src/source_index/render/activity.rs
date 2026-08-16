use serde_json::Value;

pub(crate) fn safe_activity_json(activity: &Value) -> String {
    ctx_terminal::sanitize_untrusted_history_body_for_terminal(&activity.to_string())
}

pub(crate) fn markdown_code_span(content: &str) -> String {
    let longest_backtick_run = content
        .as_bytes()
        .split(|byte| *byte != b'`')
        .map(|run| run.len())
        .max()
        .unwrap_or(0);
    let delimiter = "`".repeat(longest_backtick_run.saturating_add(1));
    format!("{delimiter}{content}{delimiter}")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{markdown_code_span, safe_activity_json};

    #[test]
    fn activity_json_escapes_bidi_and_terminal_controls_exactly() {
        let activity = json!({
            "value": "before\u{202e}\u{2066}\u{1b}\n` then `` then ``` after",
        });

        let safe = safe_activity_json(&activity);

        assert_eq!(
            safe,
            r#"{"value":"before\u{202e}\u{2066}\u001b\n` then `` then ``` after"}"#
        );
        assert!(!safe.contains('\u{202e}'));
        assert!(!safe.contains('\u{2066}'));
        assert!(!safe.contains('\u{1b}'));
    }

    #[test]
    fn markdown_code_span_exceeds_the_longest_backtick_run_without_rewriting_content() {
        let content = r#"{"single":"`","double":"``","triple":"```"}"#;

        assert_eq!(markdown_code_span(content), format!("````{content}````"));
        assert_eq!(
            markdown_code_span(r#"{"plain":true}"#),
            r#"`{"plain":true}`"#
        );
    }
}
