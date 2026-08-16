use ctx_history_core::{ActivityJsonCapture, ActivityTextCapture, CoreActivity};
use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, Copy)]
pub struct JsonlActivityObservedBytes {
    arguments: Option<u64>,
    result_text: Option<u64>,
    result_structured_content: Option<u64>,
    infer_missing: bool,
}

impl JsonlActivityObservedBytes {
    pub const fn exact(
        arguments: Option<u64>,
        result_text: Option<u64>,
        result_structured_content: Option<u64>,
    ) -> Self {
        Self {
            arguments,
            result_text,
            result_structured_content,
            infer_missing: false,
        }
    }

    pub const fn infer_from_present() -> Self {
        Self {
            arguments: None,
            result_text: None,
            result_structured_content: None,
            infer_missing: true,
        }
    }
}

/// Retains the exact activity envelope while replacing oversized complete
/// channels with explicit omission captures. Provider order and duplicate
/// literal facts are never changed.
#[inline]
pub fn fit_jsonl_activity(
    normalized_body: &str,
    structured_content: Option<&Value>,
    activity: &mut Option<CoreActivity>,
    observed: JsonlActivityObservedBytes,
    maximum_bytes: usize,
) {
    while !selected_content_fits(
        normalized_body,
        structured_content,
        activity.as_ref(),
        maximum_bytes,
    ) {
        let Some(content) = activity.as_mut() else {
            return;
        };
        let arguments_observed = observed_json_bytes(
            content
                .invocation
                .as_ref()
                .map(|invocation| &invocation.arguments),
            observed.arguments,
            observed.infer_missing,
        );
        let text_observed = observed_text_bytes(
            content.result.as_ref().map(|result| &result.text),
            observed.result_text,
            observed.infer_missing,
        );
        let structured_observed = observed_json_bytes(
            content
                .result
                .as_ref()
                .map(|result| &result.structured_content),
            observed.result_structured_content,
            observed.infer_missing,
        );

        let candidates = [
            (
                ActivityChannel::Arguments,
                content.invocation.as_ref().and_then(|invocation| {
                    json_omission_savings(&invocation.arguments, arguments_observed)
                }),
            ),
            (
                ActivityChannel::ResultText,
                content
                    .result
                    .as_ref()
                    .and_then(|result| text_omission_savings(&result.text, text_observed)),
            ),
            (
                ActivityChannel::ResultStructuredContent,
                content.result.as_ref().and_then(|result| {
                    json_omission_savings(&result.structured_content, structured_observed)
                }),
            ),
        ];
        let Some((channel, _)) = candidates
            .into_iter()
            .filter_map(|(channel, savings)| savings.map(|savings| (channel, savings)))
            .max_by_key(|(_, savings)| *savings)
        else {
            return;
        };
        match channel {
            ActivityChannel::Arguments => {
                if let Some(invocation) = content.invocation.as_mut() {
                    invocation.arguments = omitted_json(arguments_observed);
                }
            }
            ActivityChannel::ResultText => {
                if let Some(result) = content.result.as_mut() {
                    result.text = ActivityTextCapture::Omitted {
                        reason: "size_limit".to_owned(),
                        observed_bytes: text_observed,
                    };
                }
            }
            ActivityChannel::ResultStructuredContent => {
                if let Some(result) = content.result.as_mut() {
                    result.structured_content = omitted_json(structured_observed);
                }
            }
        }
    }
}

#[inline]
pub fn selected_content_fits(
    normalized_body: &str,
    structured_content: Option<&Value>,
    activity: Option<&CoreActivity>,
    maximum_bytes: usize,
) -> bool {
    normalized_body
        .len()
        .checked_add(
            structured_content
                .and_then(encoded_json_len)
                .unwrap_or_default(),
        )
        .and_then(|bytes| {
            bytes.checked_add(activity.and_then(encoded_json_len).unwrap_or_default())
        })
        .is_some_and(|bytes| bytes <= maximum_bytes)
}

#[derive(Debug, Clone, Copy)]
enum ActivityChannel {
    Arguments,
    ResultText,
    ResultStructuredContent,
}

fn observed_json_bytes(
    capture: Option<&ActivityJsonCapture>,
    exact: Option<u64>,
    infer_missing: bool,
) -> Option<u64> {
    if exact.is_some() || !infer_missing {
        return exact;
    }
    match capture? {
        ActivityJsonCapture::Present { value } => {
            encoded_json_len(value).and_then(|bytes| u64::try_from(bytes).ok())
        }
        ActivityJsonCapture::Omitted {
            observed_encoded_bytes,
            ..
        } => *observed_encoded_bytes,
        ActivityJsonCapture::Absent | ActivityJsonCapture::Unavailable => None,
    }
}

fn observed_text_bytes(
    capture: Option<&ActivityTextCapture>,
    exact: Option<u64>,
    infer_missing: bool,
) -> Option<u64> {
    if exact.is_some() || !infer_missing {
        return exact;
    }
    match capture? {
        ActivityTextCapture::Present { value } => u64::try_from(value.len()).ok(),
        ActivityTextCapture::Omitted { observed_bytes, .. } => *observed_bytes,
        ActivityTextCapture::NormalizedBody
        | ActivityTextCapture::Absent
        | ActivityTextCapture::Unavailable => None,
    }
}

fn json_omission_savings(
    capture: &ActivityJsonCapture,
    observed_encoded_bytes: Option<u64>,
) -> Option<usize> {
    if !matches!(capture, ActivityJsonCapture::Present { .. }) {
        return None;
    }
    let present = encoded_json_len(capture)?;
    let omitted = encoded_json_len(&omitted_json(observed_encoded_bytes))?;
    Some(present.saturating_sub(omitted))
}

fn text_omission_savings(
    capture: &ActivityTextCapture,
    observed_bytes: Option<u64>,
) -> Option<usize> {
    if !matches!(capture, ActivityTextCapture::Present { .. }) {
        return None;
    }
    let present = encoded_json_len(capture)?;
    let omitted = encoded_json_len(&ActivityTextCapture::Omitted {
        reason: "size_limit".to_owned(),
        observed_bytes,
    })?;
    Some(present.saturating_sub(omitted))
}

fn omitted_json(observed_encoded_bytes: Option<u64>) -> ActivityJsonCapture {
    ActivityJsonCapture::Omitted {
        reason: "size_limit".to_owned(),
        observed_encoded_bytes,
    }
}

fn encoded_json_len(value: &impl Serialize) -> Option<usize> {
    serde_json::to_vec(value).ok().map(|encoded| encoded.len())
}

#[cfg(test)]
mod tests {
    use ctx_history_core::{
        ActivityInvocation, ActivityResult, LiteralFactKind, ProviderDeclaredFact, TypedKey,
        CORE_ACTIVITY_REVISION,
    };
    use serde_json::json;

    use super::*;

    fn oversized_activity() -> Option<CoreActivity> {
        Some(CoreActivity {
            revision: CORE_ACTIVITY_REVISION,
            provider_call_id: Some(TypedKey::Utf8("call".to_owned())),
            invocation: Some(ActivityInvocation {
                protocol: Some("provider-protocol".to_owned()),
                server: Some("provider-server".to_owned()),
                tool: "tool".to_owned(),
                arguments: ActivityJsonCapture::Present {
                    value: json!({"blob": "x".repeat(4_096)}),
                },
                started_at_unix_ms: None,
            }),
            result: Some(ActivityResult {
                status: Some("provider-status".to_owned()),
                completed_at_unix_ms: None,
                duration_ns: None,
                text: ActivityTextCapture::Present {
                    value: "y".repeat(4_096),
                },
                structured_content: ActivityJsonCapture::Absent,
            }),
            facts: vec![ProviderDeclaredFact {
                kind: LiteralFactKind::File,
                value: "src/lib.rs".to_owned(),
            }],
        })
    }

    #[test]
    fn exact_observation_preserves_explicit_unknown_size() {
        let mut activity = oversized_activity();
        fit_jsonl_activity(
            "body",
            None,
            &mut activity,
            JsonlActivityObservedBytes::exact(None, None, None),
            512,
        );
        let activity = activity.unwrap();
        assert!(matches!(
            activity.invocation.unwrap().arguments,
            ActivityJsonCapture::Omitted {
                observed_encoded_bytes: None,
                ..
            }
        ));
        assert!(matches!(
            activity.result.unwrap().text,
            ActivityTextCapture::Omitted {
                observed_bytes: None,
                ..
            }
        ));
    }

    #[test]
    fn inferred_observation_records_complete_channel_sizes() {
        let mut activity = oversized_activity();
        fit_jsonl_activity(
            "body",
            None,
            &mut activity,
            JsonlActivityObservedBytes::infer_from_present(),
            512,
        );
        let activity = activity.unwrap();
        assert!(matches!(
            activity.invocation.unwrap().arguments,
            ActivityJsonCapture::Omitted {
                observed_encoded_bytes: Some(bytes),
                ..
            } if bytes > 4_096
        ));
        assert!(matches!(
            activity.result.unwrap().text,
            ActivityTextCapture::Omitted {
                observed_bytes: Some(4_096),
                ..
            }
        ));
    }
}
