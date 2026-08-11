use std::{
    io::{self, Cursor, Error, ErrorKind, Write},
    sync::{Arc, Mutex},
    time::Duration,
};

use ctx_agent_integrations::tool_backend::{
    ToolBackend, ToolBackendError, ToolExecutionError, ToolIntegrationReceipt, ToolOperation,
    ToolOutcome, ToolTransportFacts, ToolUsageFacts,
};
use ctx_history_core::CaptureProvider;
use serde_json::{json, Value};

use super::*;

#[derive(Clone, Copy)]
enum OutputFailure {
    None,
    Write,
    Flush,
}

struct TracedWriter {
    failure: OutputFailure,
    trace: Arc<Mutex<Vec<&'static str>>>,
}

impl Write for TracedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if matches!(self.failure, OutputFailure::Write) {
            self.trace.lock().unwrap().push("write_failed");
            return Err(Error::new(ErrorKind::BrokenPipe, "test write failure"));
        }
        self.trace.lock().unwrap().push("write");
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if matches!(self.failure, OutputFailure::Flush) {
            self.trace.lock().unwrap().push("flush_failed");
            return Err(Error::new(ErrorKind::BrokenPipe, "test flush failure"));
        }
        self.trace.lock().unwrap().push("flush");
        Ok(())
    }
}

struct ReceiptBackend;

impl ToolBackend for ReceiptBackend {
    fn execute(&self, _operation: ToolOperation) -> Result<ToolOutcome, ToolExecutionError> {
        Ok(ToolOutcome {
            structured: json!({"access_state": "test"}),
            compact: None,
            usage: ToolUsageFacts::default(),
            integration_receipt: Some(ToolIntegrationReceipt {
                facts: ToolTransportFacts::ProStatus {
                    access_state: Some("test".to_owned()),
                    helper_connected: true,
                    error_code: None,
                },
                success: true,
                duration: Duration::ZERO,
            }),
        })
    }

    fn invalid_blame_request(&self) -> ToolBackendError {
        panic!("pro_status test must not parse blame")
    }

    fn parse_provider(&self, _value: &str) -> Option<CaptureProvider> {
        None
    }

    fn provider_names(&self) -> Vec<&'static str> {
        Vec::new()
    }
}

struct TracedUsagePort(Arc<Mutex<Vec<&'static str>>>);

impl McpUsagePort for TracedUsagePort {
    fn record_delivered(
        &mut self,
        _operation: McpToolKind,
        _usage: ToolUsageFacts,
        _response: &Value,
        _encoded_response_bytes: usize,
        _duration: Duration,
    ) {
        self.0.lock().unwrap().push("local_usage");
    }
}

fn run_one_response(failure: OutputFailure) -> (Result<(), McpServeFailure>, Vec<&'static str>) {
    let request = serde_json::to_vec(&json!({
        "jsonrpc": "2.0",
        "id": "status",
        "method": "tools/call",
        "params": {"name": "pro_status", "arguments": {}}
    }))
    .unwrap();
    let initialized = serde_json::to_vec(&json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized"
    }))
    .unwrap();
    let mut input = Cursor::new([initialized, vec![b'\n'], request, vec![b'\n']].concat());
    let trace = Arc::new(Mutex::new(Vec::new()));
    let mut output = TracedWriter {
        failure,
        trace: trace.clone(),
    };
    let delivery_trace = trace.clone();
    let telemetry = McpTelemetry::start(true, move |events| {
        let mut trace = delivery_trace.lock().unwrap();
        for event in events {
            let label = match event {
                ctx_client_observability::analytics::PublicEventV1::OperationCompleted(event) => {
                    match &event.descriptor {
                        ctx_client_observability::operation_descriptor::OperationDescriptor::Mcp(_) => "submit_mcp",
                        ctx_client_observability::operation_descriptor::OperationDescriptor::ProHost(_) => "submit_pro",
                        _ => continue,
                    }
                }
                _ => continue,
            };
            trace.push(label);
        }
        Ok(())
    });
    let mut usage = TracedUsagePort(trace.clone());
    let result = serve_stdio(
        &mut input,
        &mut output,
        ProductIdentity {
            name: "ctx",
            version: "test",
        },
        &ReceiptBackend,
        &render_generic_tool_text,
        &mut usage,
        telemetry,
    );
    let trace = trace.lock().unwrap().clone();
    (result, trace)
}

#[test]
fn response_flush_precedes_the_one_usage_commit_and_post_flush_telemetry() {
    let (delivered, trace) = run_one_response(OutputFailure::None);
    assert!(delivered.is_ok());
    assert_eq!(
        trace
            .iter()
            .filter(|entry| **entry == "local_usage")
            .count(),
        1
    );
    let flushed_at = trace.iter().position(|entry| *entry == "flush").unwrap();
    let recorded_at = trace
        .iter()
        .position(|entry| *entry == "local_usage")
        .unwrap();
    assert!(flushed_at < recorded_at, "{trace:?}");
    for submitted in ["submit_mcp", "submit_pro"] {
        let submitted_at = trace.iter().position(|entry| *entry == submitted).unwrap();
        assert!(recorded_at < submitted_at, "{trace:?}");
    }

    for failure in [OutputFailure::Write, OutputFailure::Flush] {
        let (result, trace) = run_one_response(failure);
        assert!(matches!(
            result.unwrap_err().reason,
            McpStopReasonV1::StdoutWriteError | McpStopReasonV1::StdoutFlushError
        ));
        assert!(!trace.contains(&"local_usage"), "{trace:?}");
    }
}

#[test]
fn malformed_input_recovers_with_exact_json_rpc_parse_error() {
    let mut input =
        Cursor::new(b"\xff\n{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n".to_vec());
    let mut output = Vec::new();
    let mut usage = TracedUsagePort(Arc::new(Mutex::new(Vec::new())));
    let result = serve_stdio(
        &mut input,
        &mut output,
        ProductIdentity {
            name: "ctx",
            version: "test",
        },
        &ReceiptBackend,
        &render_generic_tool_text,
        &mut usage,
        McpTelemetry::start(false, |_| Ok(())),
    );
    assert!(result.is_ok());
    let lines = String::from_utf8(output).unwrap();
    assert!(lines.contains("MCP message is not valid UTF-8"));
    assert!(lines.contains("\"id\":null"));
    assert!(lines.contains("\"id\":1"));
}
