use std::time::Duration;

use ctx_agent_integrations::{
    mcp::{McpToolKind, RequestDescriptor},
    tool_backend::{ToolBlameTargetKind, ToolIntegrationReceipt, ToolTransportFacts},
};
use ctx_client_observability::{
    analytics::{
        pro_helper_connection_outcome, pro_operation_event, McpErrorClassV1, McpResponseBoundV1,
        McpResultMetadataV1, McpStopReasonV1, Outcome, ProAccessStateV1, ProBlameTargetV1,
        ProBlameTelemetryV1, ProHelperConnectionOutcomeV1, ProHostOperationV1,
        ProStatusTelemetryV1, ProSurfaceV1, PublicEventV1,
    },
    mcp_observation::{
        McpDeliveredResponse, McpObservation, McpObservedTool, McpRequestObservation,
    },
    operation_descriptor::ObservedMcpProductOperation,
};
use serde_json::Value;

fn observed_operation(kind: McpToolKind) -> Option<ObservedMcpProductOperation> {
    match kind {
        McpToolKind::Status => Some(ObservedMcpProductOperation::Status),
        McpToolKind::Sources => Some(ObservedMcpProductOperation::Sources),
        McpToolKind::Search => Some(ObservedMcpProductOperation::Search),
        McpToolKind::ShowSession => Some(ObservedMcpProductOperation::ShowSession),
        McpToolKind::ShowEvent => Some(ObservedMcpProductOperation::ShowEvent),
        McpToolKind::QueryEvents => Some(ObservedMcpProductOperation::QueryEvents),
        McpToolKind::Blame => Some(ObservedMcpProductOperation::Blame),
        McpToolKind::ProStatus => Some(ObservedMcpProductOperation::ProStatus),
        McpToolKind::Unknown | McpToolKind::Missing => None,
    }
}

fn request_observation(descriptor: RequestDescriptor) -> McpRequestObservation {
    match descriptor {
        RequestDescriptor::Initialize => McpRequestObservation::Initialize,
        RequestDescriptor::Ping => McpRequestObservation::Ping,
        RequestDescriptor::ToolsList => McpRequestObservation::ToolsList,
        RequestDescriptor::ToolCall { operation } => {
            McpRequestObservation::ToolCall(match observed_operation(operation) {
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

pub struct McpTelemetry {
    observation: Option<McpObservation>,
}

impl McpTelemetry {
    /// Starts telemetry only after the product has authorized it. The injected
    /// delivery port may re-check opt-out immediately before sending a batch.
    pub fn start(
        authorized: bool,
        dispatch: impl Fn(&[PublicEventV1]) -> Result<(), ()> + Send + Sync + 'static,
    ) -> Self {
        Self {
            observation: authorized.then(|| McpObservation::start(dispatch)),
        }
    }

    pub fn record_delivered(
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

    pub fn record_response_failure(
        &mut self,
        descriptor: RequestDescriptor,
        duration: Duration,
        class: McpErrorClassV1,
    ) {
        if let Some(observation) = &mut self.observation {
            observation.record_response_failure(request_observation(descriptor), duration, class);
        }
    }

    pub fn submit_backend_receipt(&self, receipt: ToolIntegrationReceipt) {
        if let Some(observation) = &self.observation {
            observation.submit_post_flush_event(backend_receipt_event(receipt));
        }
    }

    pub fn stop(mut self, reason: McpStopReasonV1, outcome: Outcome, duration: Duration) {
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
            metadata.response_bound = Some(
                if result.get("error_code").and_then(Value::as_str) == Some("output_limit_exceeded")
                {
                    McpResponseBoundV1::Replaced
                } else {
                    McpResponseBoundV1::WithinLimit
                },
            );
        }
        McpToolKind::QueryEvents => {
            if let Some(count) = result.get("events").and_then(Value::as_array).map(Vec::len) {
                metadata = metadata.with_result_count(count);
            }
            metadata.result_truncated = result.get("truncated").and_then(Value::as_bool);
            metadata.response_bound = Some(
                if result.get("error_code").and_then(Value::as_str) == Some("output_limit_exceeded")
                {
                    McpResponseBoundV1::Replaced
                } else {
                    McpResponseBoundV1::WithinLimit
                },
            );
        }
        McpToolKind::Blame
        | McpToolKind::ProStatus
        | McpToolKind::Status
        | McpToolKind::Unknown
        | McpToolKind::Missing => {}
    }
    metadata
}

fn backend_receipt_event(receipt: ToolIntegrationReceipt) -> PublicEventV1 {
    let operation = match receipt.facts {
        ToolTransportFacts::ProStatus {
            access_state,
            helper_connected,
            error_code,
        } => {
            let mut telemetry = ProStatusTelemetryV1::new(ProSurfaceV1::Mcp);
            telemetry.access_state = access_state
                .as_deref()
                .and_then(ProAccessStateV1::from_safe_name);
            telemetry.helper_connection = if helper_connected {
                ProHelperConnectionOutcomeV1::Connected
            } else {
                pro_helper_connection_outcome(error_code.as_deref())
            };
            if error_code.is_some() {
                telemetry.fail(error_code.as_deref());
            }
            ProHostOperationV1::Status(telemetry)
        }
        ToolTransportFacts::Blame {
            target,
            result_count,
            has_more,
            failure_code,
        } => {
            let target = target.map(|target| match target {
                ToolBlameTargetKind::File => ProBlameTargetV1::File,
                ToolBlameTargetKind::Commit => ProBlameTargetV1::Commit,
                ToolBlameTargetKind::PullRequest => ProBlameTargetV1::PullRequest,
            });
            let mut telemetry = ProBlameTelemetryV1::new(target, ProSurfaceV1::Mcp);
            if receipt.success {
                telemetry.complete(result_count.unwrap_or(0), has_more.unwrap_or(false));
            } else {
                telemetry.fail(failure_code);
            }
            ProHostOperationV1::Blame(telemetry)
        }
    };
    pro_operation_event(
        operation,
        if receipt.success {
            Outcome::Success
        } else {
            Outcome::Failure
        },
        receipt.duration,
    )
}

#[cfg(test)]
mod tests;
