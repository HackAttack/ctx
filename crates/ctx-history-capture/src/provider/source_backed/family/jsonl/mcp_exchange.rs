use ctx_history_core::{McpExchangeContent, McpJsonCapture, McpPayloadOmissionReason};
use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, Copy)]
pub(crate) struct JsonlMcpObservedEncodedBytes {
    arguments: Option<u64>,
    payload: Option<u64>,
    infer_missing: bool,
}

impl JsonlMcpObservedEncodedBytes {
    pub(crate) const fn exact(arguments: Option<u64>, payload: Option<u64>) -> Self {
        Self {
            arguments,
            payload,
            infer_missing: false,
        }
    }

    pub(crate) const fn infer_from_present() -> Self {
        Self {
            arguments: None,
            payload: None,
            infer_missing: true,
        }
    }
}

/// Retains the most informative MCP exchange that fits beside the provider's
/// authoritative selected content. Arguments and response payload are omitted
/// in descending savings order; if the explicit omission shape still cannot
/// fit, only the optional exchange is dropped.
#[inline]
pub(crate) fn fit_jsonl_mcp_exchange(
    normalized_body: &str,
    structured_content: Option<&Value>,
    exchange: &mut Option<McpExchangeContent>,
    observed: JsonlMcpObservedEncodedBytes,
    maximum_bytes: usize,
) {
    while !selected_content_fits(
        normalized_body,
        structured_content,
        exchange.as_ref(),
        maximum_bytes,
    ) {
        let Some(content) = exchange.as_mut() else {
            return;
        };
        let arguments_observed = observed_bytes(
            content
                .invocation
                .as_ref()
                .map(|invocation| &invocation.arguments),
            observed.arguments,
            observed.infer_missing,
        );
        let payload_observed = observed_bytes(
            content.response.as_ref().map(|response| &response.payload),
            observed.payload,
            observed.infer_missing,
        );
        match (
            content
                .invocation
                .as_ref()
                .and_then(|invocation| omission_savings(&invocation.arguments, arguments_observed)),
            content
                .response
                .as_ref()
                .and_then(|response| omission_savings(&response.payload, payload_observed)),
        ) {
            (Some(arguments), Some(payload)) if arguments >= payload => {
                omit_arguments(content, arguments_observed)
            }
            (Some(_), Some(_)) => omit_payload(content, payload_observed),
            (Some(_), None) => omit_arguments(content, arguments_observed),
            (None, Some(_)) => omit_payload(content, payload_observed),
            (None, None) => *exchange = None,
        }
    }
}

#[inline]
pub(crate) fn selected_content_fits(
    normalized_body: &str,
    structured_content: Option<&Value>,
    exchange: Option<&McpExchangeContent>,
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
            bytes.checked_add(exchange.and_then(encoded_json_len).unwrap_or_default())
        })
        .is_some_and(|bytes| bytes <= maximum_bytes)
}

fn observed_bytes(
    capture: Option<&McpJsonCapture>,
    exact: Option<u64>,
    infer_missing: bool,
) -> Option<u64> {
    if exact.is_some() || !infer_missing {
        return exact;
    }
    match capture? {
        McpJsonCapture::Present { value } => {
            encoded_json_len(value).and_then(|bytes| u64::try_from(bytes).ok())
        }
        McpJsonCapture::Omitted {
            observed_encoded_bytes,
            ..
        } => *observed_encoded_bytes,
        McpJsonCapture::Absent | McpJsonCapture::Unavailable => None,
    }
}

fn omission_savings(
    capture: &McpJsonCapture,
    observed_encoded_bytes: Option<u64>,
) -> Option<usize> {
    if !matches!(capture, McpJsonCapture::Present { .. }) {
        return None;
    }
    let present = encoded_json_len(capture)?;
    let omitted = encoded_json_len(&McpJsonCapture::Omitted {
        reason: McpPayloadOmissionReason::SizeLimit,
        observed_encoded_bytes,
    })?;
    Some(present.saturating_sub(omitted))
}

fn omit_arguments(content: &mut McpExchangeContent, observed_encoded_bytes: Option<u64>) {
    if let Some(invocation) = content.invocation.as_mut() {
        invocation.arguments = omitted(observed_encoded_bytes);
    }
}

fn omit_payload(content: &mut McpExchangeContent, observed_encoded_bytes: Option<u64>) {
    if let Some(response) = content.response.as_mut() {
        response.payload = omitted(observed_encoded_bytes);
    }
}

fn omitted(observed_encoded_bytes: Option<u64>) -> McpJsonCapture {
    McpJsonCapture::Omitted {
        reason: McpPayloadOmissionReason::SizeLimit,
        observed_encoded_bytes,
    }
}

fn encoded_json_len(value: &impl Serialize) -> Option<usize> {
    serde_json::to_vec(value).ok().map(|encoded| encoded.len())
}

#[cfg(test)]
mod tests {
    use ctx_history_core::{McpInvocationContent, McpJsonCapture};
    use serde_json::json;

    use super::*;

    fn oversized_exchange() -> Option<McpExchangeContent> {
        Some(McpExchangeContent {
            provider_call_id: "call".to_owned(),
            invocation: Some(McpInvocationContent {
                server: "server".to_owned(),
                tool: "tool".to_owned(),
                arguments: McpJsonCapture::Present {
                    value: json!({"blob": "x".repeat(4_096)}),
                },
            }),
            response: None,
        })
    }

    #[test]
    fn exact_observation_preserves_provider_evidence() {
        let mut exchange = oversized_exchange();
        fit_jsonl_mcp_exchange(
            "body",
            None,
            &mut exchange,
            JsonlMcpObservedEncodedBytes::exact(None, None),
            256,
        );
        assert!(matches!(
            exchange.unwrap().invocation.unwrap().arguments,
            McpJsonCapture::Omitted {
                observed_encoded_bytes: None,
                ..
            }
        ));
    }

    #[test]
    fn inferred_observation_records_present_json_size() {
        let mut exchange = oversized_exchange();
        fit_jsonl_mcp_exchange(
            "body",
            None,
            &mut exchange,
            JsonlMcpObservedEncodedBytes::infer_from_present(),
            256,
        );
        assert!(matches!(
            exchange.unwrap().invocation.unwrap().arguments,
            McpJsonCapture::Omitted {
                observed_encoded_bytes: Some(bytes),
                ..
            } if bytes > 4_096
        ));
    }
}
