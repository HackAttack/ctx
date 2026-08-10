use std::{path::PathBuf, time::Duration};

use serde_json::Value;

use crate::{
    analytics::{
        McpErrorClassV1, McpResponseBoundV1, McpResultMetadataV1, McpStopReasonV1, Outcome,
        PublicEventV1,
    },
    config::AppConfig,
    operation_descriptor::McpOperationKind,
};
use ctx_client_observability::mcp_observation::{
    McpDeliveredResponse, McpObservation, McpObservedTool, McpRequestObservation,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RequestDescriptor {
    Initialize,
    Ping,
    ToolsList,
    ToolCall { operation: McpOperationKind },
    UnknownRequest,
    MissingRequest,
    InitializedNotification,
    UnknownNotification,
    InvalidJson,
    InvalidUtf8,
    LineTooLarge,
}

impl RequestDescriptor {
    pub(super) fn from_message(message: &Value) -> Self {
        let Some(object) = message.as_object() else {
            return Self::MissingRequest;
        };
        let method = object.get("method").and_then(Value::as_str);
        if !object.contains_key("id") {
            return if method == Some("notifications/initialized") {
                Self::InitializedNotification
            } else {
                Self::UnknownNotification
            };
        }
        match method {
            Some("initialize") => Self::Initialize,
            Some("ping") => Self::Ping,
            Some("tools/list") => Self::ToolsList,
            Some("tools/call") => Self::ToolCall {
                operation: McpOperationKind::from_tool_name(
                    message.pointer("/params/name").and_then(Value::as_str),
                ),
            },
            Some(_) => Self::UnknownRequest,
            None => Self::MissingRequest,
        }
    }

    fn observation(self) -> McpRequestObservation {
        match self {
            Self::Initialize => McpRequestObservation::Initialize,
            Self::Ping => McpRequestObservation::Ping,
            Self::ToolsList => McpRequestObservation::ToolsList,
            Self::ToolCall { operation } => {
                McpRequestObservation::ToolCall(match operation.observed() {
                    Some(operation) => McpObservedTool::Product(operation),
                    None if operation == McpOperationKind::Unknown => McpObservedTool::Unknown,
                    None => McpObservedTool::Missing,
                })
            }
            Self::UnknownRequest => McpRequestObservation::UnknownRequest,
            Self::MissingRequest => McpRequestObservation::MissingRequest,
            Self::InitializedNotification => McpRequestObservation::InitializedNotification,
            Self::UnknownNotification => McpRequestObservation::UnknownNotification,
            Self::InvalidJson => McpRequestObservation::InvalidJson,
            Self::InvalidUtf8 => McpRequestObservation::InvalidUtf8,
            Self::LineTooLarge => McpRequestObservation::LineTooLarge,
        }
    }
}

pub(super) struct McpHandled<T> {
    pub(super) value: T,
    pub(super) pro_event: Option<PublicEventV1>,
}

impl<T> McpHandled<T> {
    pub(super) fn plain(value: T) -> Self {
        Self {
            value,
            pro_event: None,
        }
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
        observation.record_delivered(descriptor.observation(), delivered, duration);
    }

    pub(super) fn record_response_failure(
        &mut self,
        descriptor: RequestDescriptor,
        duration: Duration,
        class: McpErrorClassV1,
    ) {
        if let Some(observation) = &mut self.observation {
            observation.record_response_failure(descriptor.observation(), duration, class);
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
            operation: McpOperationKind::Missing
        }
    ) {
        return McpErrorClassV1::MissingTool;
    }
    if matches!(
        descriptor,
        RequestDescriptor::ToolCall {
            operation: McpOperationKind::Unknown
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

fn result_metadata(operation: McpOperationKind, response: &Value) -> McpResultMetadataV1 {
    let Some(result) = response.pointer("/result/structuredContent") else {
        return McpResultMetadataV1::default();
    };
    let mut metadata = McpResultMetadataV1::default();
    match operation {
        McpOperationKind::Sources => {
            if let Some(count) = result
                .get("sources")
                .and_then(Value::as_array)
                .map(Vec::len)
            {
                metadata = metadata.with_result_count(count);
            }
        }
        McpOperationKind::Search => {
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
        McpOperationKind::ShowSession | McpOperationKind::ShowEvent => {
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
        McpOperationKind::QueryEvents => {
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
        McpOperationKind::Blame
        | McpOperationKind::ProStatus
        | McpOperationKind::Status
        | McpOperationKind::Unknown
        | McpOperationKind::Missing => {}
    }
    metadata
}

#[cfg(test)]
mod tests;
