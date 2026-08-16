pub(crate) mod native_path;
mod normalization;
mod relationships;
mod source;

const SHELLEY_CAPTURE_REVISION: u32 = 11;
const SHELLEY_POLICY_REVISION: u32 = 7;
const SHELLEY_MESSAGE_VALUE_COUNT: usize = 15;
const SHELLEY_CONVERSATION_VALUE_COUNT: usize = 17;

#[cfg(test)]
mod tests {
    use super::native_path::source_backed::shelley_literal_status;
    use serde_json::json;

    #[test]
    fn shelley_tool_result_status_preserves_success_failure_and_timeout() {
        assert_eq!(
            shelley_literal_status(&json!({"status": "success"})).as_deref(),
            Some("success")
        );
        assert_eq!(
            shelley_literal_status(&json!({"state": "failure"})).as_deref(),
            Some("failure")
        );
        assert_eq!(shelley_literal_status(&json!({"timed_out": true})), None);
        assert_eq!(
            shelley_literal_status(&json!({"status": "one", "outcome": "two"})),
            None
        );
    }
}
