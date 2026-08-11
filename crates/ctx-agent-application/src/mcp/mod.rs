//! Newline-delimited MCP application delivery, independent of process setup.

use std::{
    io::{BufRead, Write},
    time::{Duration, Instant},
};

use anyhow::Error;
use ctx_agent_integrations::{
    mcp::{
        encode_response_line, error_response, handle_protocol_message, read_mcp_input_line,
        McpInputLine, McpServerIdentity, McpToolKind, McpUsage, RequestDescriptor,
        MCP_MAX_LINE_BYTES,
    },
    tool_backend::{ToolBackend, ToolUsageFacts},
};
use ctx_client_observability::analytics::{McpErrorClassV1, McpStopReasonV1, Outcome};
use serde_json::{json, Value};

mod telemetry;
mod text;

pub use telemetry::McpTelemetry;
pub use text::render_generic_tool_text;

/// Product-owned MCP identity. The application crate never derives it from its
/// own package metadata, because wire identity belongs to the executable.
#[derive(Debug, Clone, Copy)]
pub struct ProductIdentity<'a> {
    pub name: &'a str,
    pub version: &'a str,
}

/// Product-owned compact text projection for MCP tool results.
pub trait McpTextPort {
    fn render_tool_text(&self, value: &Value) -> String;
}

impl<F> McpTextPort for F
where
    F: Fn(&Value) -> String,
{
    fn render_tool_text(&self, value: &Value) -> String {
        self(value)
    }
}

/// Product-owned local usage persistence. The application invokes this once,
/// only after the exact JSON-RPC response has been written and flushed.
pub trait McpUsagePort {
    fn record_delivered(
        &mut self,
        operation: McpToolKind,
        usage: ToolUsageFacts,
        response: &Value,
        encoded_response_bytes: usize,
        duration: Duration,
    );
}

/// A transport failure classified for content-free MCP lifecycle telemetry.
#[derive(Debug)]
pub struct McpServeFailure {
    pub reason: McpStopReasonV1,
    error: Error,
}

impl McpServeFailure {
    pub fn into_error(self) -> Error {
        self.error
    }
}

/// Serves MCP on already-opened stdio ports. Process setup, configuration,
/// daemon policy, concrete backends, usage storage, and error presentation stay
/// with the product adapter.
pub fn serve_stdio<B, T, U>(
    stdin: &mut impl BufRead,
    stdout: &mut impl Write,
    identity: ProductIdentity<'_>,
    backend: &B,
    text: &T,
    usage: &mut U,
    mut telemetry: McpTelemetry,
) -> Result<(), McpServeFailure>
where
    B: ToolBackend,
    T: McpTextPort,
    U: McpUsagePort,
{
    let started = Instant::now();
    let mut initialized = false;
    let result = serve_stdio_loop(
        stdin,
        stdout,
        identity,
        backend,
        text,
        usage,
        &mut telemetry,
        &mut initialized,
    );
    let (reason, outcome) = match &result {
        Ok(()) => (McpStopReasonV1::Eof, Outcome::Success),
        Err(failure) => (failure.reason, Outcome::Failure),
    };
    telemetry.stop(reason, outcome, started.elapsed());
    result
}

#[allow(
    clippy::too_many_arguments,
    reason = "the ports preserve the one-pass stdio loop"
)]
fn serve_stdio_loop<B, T, U>(
    stdin: &mut impl BufRead,
    stdout: &mut impl Write,
    identity: ProductIdentity<'_>,
    backend: &B,
    text: &T,
    usage_port: &mut U,
    telemetry: &mut McpTelemetry,
    initialized: &mut bool,
) -> Result<(), McpServeFailure>
where
    B: ToolBackend,
    T: McpTextPort,
    U: McpUsagePort,
{
    loop {
        let input = read_mcp_input_line(stdin).map_err(|error| McpServeFailure {
            reason: McpStopReasonV1::StdinReadError,
            error,
        })?;
        let Some(input) = input else {
            return Ok(());
        };
        let request_started = Instant::now();
        let (handled, descriptor) = match input {
            McpInputLine::Line(line) => {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                match serde_json::from_str::<Value>(line) {
                    Ok(message) => {
                        let descriptor = RequestDescriptor::from_message(&message);
                        (
                            handle_protocol_message(
                                message,
                                descriptor,
                                initialized,
                                McpServerIdentity {
                                    name: identity.name,
                                    version: identity.version,
                                },
                                backend,
                                |value| text.render_tool_text(value),
                            ),
                            descriptor,
                        )
                    }
                    Err(error) => (
                        ctx_agent_integrations::mcp::McpHandled::plain(Some(error_response(
                            Value::Null,
                            -32700,
                            "Parse error",
                            Some(json!({ "error": error.to_string() })),
                        ))),
                        RequestDescriptor::InvalidJson,
                    ),
                }
            }
            McpInputLine::InvalidUtf8 => (
                ctx_agent_integrations::mcp::McpHandled::plain(Some(error_response(
                    Value::Null,
                    -32700,
                    "Parse error",
                    Some(json!({ "error": "MCP message is not valid UTF-8" })),
                ))),
                RequestDescriptor::InvalidUtf8,
            ),
            McpInputLine::TooLarge => (
                ctx_agent_integrations::mcp::McpHandled::plain(Some(error_response(
                    Value::Null,
                    -32700,
                    "Parse error",
                    Some(json!({
                        "error": format!("MCP message exceeds max line bytes ({MCP_MAX_LINE_BYTES})")
                    })),
                ))),
                RequestDescriptor::LineTooLarge,
            ),
        };
        let response = handled.value;
        if let Some(response) = response {
            let encoded = encode_response_line(&response).map_err(|error| {
                telemetry.record_response_failure(
                    descriptor,
                    request_started.elapsed(),
                    McpErrorClassV1::ResponseSerialize,
                );
                McpServeFailure {
                    reason: McpStopReasonV1::ResponseSerializeError,
                    error: error.into(),
                }
            })?;
            stdout.write_all(encoded.as_bytes()).map_err(|error| {
                telemetry.record_response_failure(
                    descriptor,
                    request_started.elapsed(),
                    McpErrorClassV1::ResponseWrite,
                );
                McpServeFailure {
                    reason: McpStopReasonV1::StdoutWriteError,
                    error: error.into(),
                }
            })?;
            stdout.flush().map_err(|error| {
                telemetry.record_response_failure(
                    descriptor,
                    request_started.elapsed(),
                    McpErrorClassV1::ResponseFlush,
                );
                McpServeFailure {
                    reason: McpStopReasonV1::StdoutFlushError,
                    error: error.into(),
                }
            })?;
            let duration = request_started.elapsed();
            if let Some(McpUsage { operation, facts }) = handled.usage {
                usage_port.record_delivered(operation, facts, &response, encoded.len(), duration);
            }
            telemetry.record_delivered(descriptor, Some(&response), duration);
            if let Some(receipt) = handled.integration_receipt {
                telemetry.submit_backend_receipt(receipt);
            }
        } else {
            telemetry.record_delivered(descriptor, None, request_started.elapsed());
        }
    }
}

#[cfg(test)]
mod tests;
