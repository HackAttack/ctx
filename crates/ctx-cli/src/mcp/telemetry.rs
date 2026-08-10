use std::{path::PathBuf, time::Duration};

use ctx_agent_integrations::mcp::{McpToolKind, RequestDescriptor};
use serde_json::Value;

use crate::{
    analytics::{
        McpErrorClassV1, McpResponseBoundV1, McpResultMetadataV1, McpStopReasonV1, Outcome,
        PublicEventV1,
    },
    config::AppConfig,
    operation_descriptor::observed_mcp_product_operation,
};
use ctx_client_observability::mcp_observation::{
    McpDeliveredResponse, McpObservation, McpObservedTool, McpRequestObservation,
};

fn request_observation(descriptor: RequestDescriptor) -> McpRequestObservation {
    match descriptor {
        RequestDescriptor::Initialize => McpRequestObservation::Initialize,
        RequestDescriptor::Ping => McpRequestObservation::Ping,
        RequestDescriptor::ToolsList => McpRequestObservation::ToolsList,
        RequestDescriptor::ToolCall { operation } => {
            McpRequestObservation::ToolCall(match observed_mcp_product_operation(operation) {
                Some(operation) => McpObservedTool::Product(operation),
                None if operation == McpToolKind::Unknown => McpObservedTool::Unknown,
                None => McpObservedTool::Missing,
            })
        }
        RequestDescriptor::UnknownRequest => McpRequestObservation::UnknownRequest,
        RequestDescriptor::MissingRequest => McpRequestObservation::MissingRequest,
        RequestDescriptor::InitializedNotification => {
            McpRequestObservation::InitializedNotification
        }
        RequestDescriptor::UnknownNotification => McpRequestObservation::UnknownNotification,
        RequestDescriptor::InvalidJson => McpRequestObservation::InvalidJson,
        RequestDescriptor::InvalidUtf8 => McpRequestObservation::InvalidUtf8,
        RequestDescriptor::LineTooLarge => McpRequestObservation::LineTooLarge,
    }
}

pub(super) struct McpTelemetry {
    observation: Option<McpObservation>,
}

impl McpTelemetry {
    pub(super) fn start(data_root: PathBuf) -> Self {
        let enabled = AppConfig::load(&data_root).is_ok_and(|config| config.analytics.enabled);
        if !enabled {
            return Self { observation: None };
        }
        let delivery_root = data_root.clone();
        Self {
            observation: Some(McpObservation::start(move |events| {
                let Ok(config) = AppConfig::load(&delivery_root) else {
                    return Ok(());
                };
                if !config.analytics.enabled {
                    return Ok(());
                }
                crate::analytics::send_batch(&delivery_root, &config, events);
                Ok(())
            })),
        }
    }

    #[cfg(test)]
    pub(super) fn start_for_test(
        dispatch: impl Fn(&[PublicEventV1]) -> Result<(), ()> + Send + Sync + 'static,
    ) -> Self {
        Self {
            observation: Some(McpObservation::start(dispatch)),
        }
    }

    pub(super) fn record_delivered(
        &mut self,
        descriptor: RequestDescriptor,
        response: Option<&Value>,
        duration: Duration,
    ) {
        let Some(observation) = &mut self.observation else {
            return;
        };
        let delivered = response.map(|response| delivered_response(descriptor, response));
        observation.record_delivered(request_observation(descriptor), delivered, duration);
    }

    pub(super) fn record_response_failure(
        &mut self,
        descriptor: RequestDescriptor,
        duration: Duration,
        class: McpErrorClassV1,
    ) {
        if let Some(observation) = &mut self.observation {
            observation.record_response_failure(request_observation(descriptor), duration, class);
        }
    }

    pub(super) fn submit_pro_event(&self, event: PublicEventV1) {
        if let Some(observation) = &self.observation {
            observation.submit_post_flush_event(event);
        }
    }

    pub(super) fn stop(mut self, reason: McpStopReasonV1, outcome: Outcome, duration: Duration) {
        if let Some(observation) = self.observation.take() {
            observation.stop(reason, outcome, duration);
        }
    }
}

fn delivered_response(descriptor: RequestDescriptor, response: &Value) -> McpDeliveredResponse {
    let error_class = response
        .get("error")
        .map(|error| json_rpc_error_class(descriptor, error));
    let tool_error = response.pointer("/result/isError").and_then(Value::as_bool) == Some(true);
    let result = match descriptor {
        RequestDescriptor::ToolCall { operation } => result_metadata(operation, response),
        _ => McpResultMetadataV1::default(),
    };
    McpDeliveredResponse {
        error_class,
        tool_error,
        result,
    }
}

fn json_rpc_error_class(descriptor: RequestDescriptor, error: &Value) -> McpErrorClassV1 {
    if descriptor == RequestDescriptor::InvalidUtf8 {
        return McpErrorClassV1::InvalidUtf8;
    }
    if descriptor == RequestDescriptor::LineTooLarge {
        return McpErrorClassV1::LineTooLarge;
    }
    if descriptor == RequestDescriptor::InvalidJson {
        return McpErrorClassV1::InvalidJson;
    }
    if matches!(
        descriptor,
        RequestDescriptor::ToolCall {
            operation: McpToolKind::Missing
        }
    ) {
        return McpErrorClassV1::MissingTool;
    }
    if matches!(
        descriptor,
        RequestDescriptor::ToolCall {
            operation: McpToolKind::Unknown
        }
    ) {
        return McpErrorClassV1::UnknownTool;
    }
    match error.get("code").and_then(Value::as_i64) {
        Some(-32700) => McpErrorClassV1::InvalidJson,
        Some(-32600) => McpErrorClassV1::InvalidRequest,
        Some(-32602) => McpErrorClassV1::InvalidParams,
        Some(-32002) => McpErrorClassV1::ServerNotInitialized,
        Some(-32601) => McpErrorClassV1::MethodNotFound,
        _ => McpErrorClassV1::InvalidRequest,
    }
}

fn result_metadata(operation: McpToolKind, response: &Value) -> McpResultMetadataV1 {
    let Some(result) = response.pointer("/result/structuredContent") else {
        return McpResultMetadataV1::default();
    };
    let mut metadata = McpResultMetadataV1::default();
    match operation {
        McpToolKind::Sources => {
            if let Some(count) = result
                .get("sources")
                .and_then(Value::as_array)
                .map(Vec::len)
            {
                metadata = metadata.with_result_count(count);
            }
        }
        McpToolKind::Search => {
            if let Some(count) = result
                .get("results")
                .and_then(Value::as_array)
                .map(Vec::len)
            {
                metadata = metadata.with_result_count(count);
            }
            let truncated = result
                .pointer("/truncation/truncated")
                .and_then(Value::as_bool);
            let has_more = result
                .pointer("/pagination/has_more")
                .and_then(Value::as_bool);
            metadata.result_truncated = match (truncated, has_more) {
                (Some(a), Some(b)) => Some(a || b),
                (value @ Some(_), None) | (None, value @ Some(_)) => value,
                (None, None) => None,
            };
        }
        McpToolKind::ShowSession | McpToolKind::ShowEvent => {
            if let Some(count) = result.get("events").and_then(Value::as_array).map(Vec::len) {
                metadata = metadata.with_result_count(count);
            }
            metadata.events_truncated =
                result.pointer("/truncated/events").and_then(Value::as_bool);
            if response.get("result").is_some() {
                metadata.response_bound = Some(
                    if result.get("error_code").and_then(Value::as_str)
                        == Some("output_limit_exceeded")
                    {
                        McpResponseBoundV1::Replaced
                    } else {
                        McpResponseBoundV1::WithinLimit
                    },
                );
            }
        }
        McpToolKind::QueryEvents => {
            if let Some(count) = result.get("events").and_then(Value::as_array).map(Vec::len) {
                metadata = metadata.with_result_count(count);
            }
            metadata.result_truncated = result.get("truncated").and_then(Value::as_bool);
            if response.get("result").is_some() {
                metadata.response_bound = Some(
                    if result.get("error_code").and_then(Value::as_str)
                        == Some("output_limit_exceeded")
                    {
                        McpResponseBoundV1::Replaced
                    } else {
                        McpResponseBoundV1::WithinLimit
                    },
                );
            }
        }
        McpToolKind::Blame
        | McpToolKind::ProStatus
        | McpToolKind::Status
        | McpToolKind::Unknown
        | McpToolKind::Missing => {}
    }
    metadata
}

#[cfg(test)]
mod tests;
