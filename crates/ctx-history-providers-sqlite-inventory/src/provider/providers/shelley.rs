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
    use super::{normalization::shelley_output_classification, relationships::ShelleyMessageRow};
    use crate::OutputOutcome;

    fn output_message(payload: &str) -> ShelleyMessageRow {
        ShelleyMessageRow {
            rowid: 1,
            message_id: "message".to_owned(),
            conversation_id: "conversation".to_owned(),
            sequence_id: 1,
            entry_type: "tool".to_owned(),
            llm_data: Some(payload.to_owned()),
            user_data: None,
            usage_data: None,
            created_at: None,
            display_data: None,
            excluded_from_context: false,
            generation: None,
            llm_api_url: None,
            model_name: None,
            forked_from_message_id: None,
        }
    }

    #[test]
    fn shelley_tool_result_status_preserves_success_failure_and_timeout() {
        for (payload, expected) in [
            (
                r#"{"Type":"tool_result","status":"success"}"#,
                OutputOutcome::Success,
            ),
            (
                r#"{"Type":"tool_result","exit_code":7}"#,
                OutputOutcome::Failure,
            ),
            (
                r#"{"Type":"tool_result","timed_out":true}"#,
                OutputOutcome::Timeout,
            ),
        ] {
            let classification = shelley_output_classification(&output_message(payload))
                .expect("tool output classification");
            assert_eq!(classification.outcome, expected);
        }
    }
}
