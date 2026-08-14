use std::{borrow::Cow, fmt};

use serde::{
    de::{MapAccess, SeqAccess, Visitor},
    Deserialize, Deserializer,
};
use serde_json::{value::RawValue, Value};

use super::mcp::BoundedStringProbe;
use super::*;
use crate::common::json::raw_object_keys_are_unique;
use crate::provider::source_backed::family::jsonl::{
    fit_jsonl_mcp_exchange, jsonl_selected_content_fits, JsonlMcpObservedEncodedBytes,
};

pub(super) struct ProjectedMcpExchange {
    content: ctx_history_core::McpExchangeContent,
    arguments_observed_encoded_bytes: Option<u64>,
    payload_observed_encoded_bytes: Option<u64>,
    strict_discovery_payload: bool,
}

pub(super) struct ProjectedMcpTerminal {
    attribution: Option<ctx_history_core::McpToolCallAttribution>,
    exchange: Option<ProjectedMcpExchange>,
}

impl ProjectedMcpTerminal {
    pub(super) fn into_parts(
        self,
    ) -> (
        Option<ctx_history_core::McpToolCallAttribution>,
        Option<ProjectedMcpExchange>,
    ) {
        (self.attribution, self.exchange)
    }
}

impl ProjectedMcpExchange {
    /// Retains as much exact typed exchange content as fits beside the selected
    /// normalized body. Larger JSON channels become explicit omissions; the
    /// normalized body itself is never shortened or replaced.
    pub(super) fn fit_selected_body(mut self, normalized_body: &str) -> Option<Self> {
        let mut exchange = Some(self.content);
        fit_jsonl_mcp_exchange(
            normalized_body,
            None,
            &mut exchange,
            JsonlMcpObservedEncodedBytes::exact(
                self.arguments_observed_encoded_bytes,
                self.payload_observed_encoded_bytes,
            ),
            ctx_history_core::MAX_CORE_CONTENT_BYTES,
        );
        self.content = exchange?;
        Some(self)
    }

    pub(super) fn content(&self) -> &ctx_history_core::McpExchangeContent {
        &self.content
    }

    pub(super) fn into_content(self) -> ctx_history_core::McpExchangeContent {
        self.content
    }

    pub(super) fn discovery_exclusion(
        &self,
        source_unique_terminal: bool,
    ) -> Option<ctx_history_core::CoreDiscoveryExclusion> {
        let invocation = self.content.invocation.as_ref();
        let linked_invocation =
            source_unique_terminal
                .then_some(invocation)
                .flatten()
                .map(|invocation| {
                    ctx_history_capture_model::ctx_retrieval::classify_mcp_invocation(
                        &invocation.server,
                        &invocation.tool,
                    )
                });
        let terminal_status = self
            .content
            .response
            .as_ref()
            .map(|response| match response.status {
                ctx_history_core::McpTerminalStatus::Succeeded => {
                    ctx_history_capture_model::ctx_retrieval::ResultTerminalStatus::Succeeded
                }
                ctx_history_core::McpTerminalStatus::Failed
                | ctx_history_core::McpTerminalStatus::Cancelled
                | ctx_history_core::McpTerminalStatus::TimedOut => {
                    ctx_history_capture_model::ctx_retrieval::ResultTerminalStatus::Failed
                }
                ctx_history_core::McpTerminalStatus::Unknown => {
                    ctx_history_capture_model::ctx_retrieval::ResultTerminalStatus::Unknown
                }
            })
            .unwrap_or(ctx_history_capture_model::ctx_retrieval::ResultTerminalStatus::Unknown);
        let atom = if self.strict_discovery_payload {
            ctx_history_capture_model::ctx_retrieval::ResultAtom::Payload
        } else {
            ctx_history_capture_model::ctx_retrieval::ResultAtom::Unknown
        };
        let contribution = ctx_history_capture_model::ctx_retrieval::classify_linked_result(
            linked_invocation,
            terminal_status,
            [atom],
        );
        ctx_history_capture_model::ctx_retrieval::discovery_exclusion_for([contribution])
    }
}

pub(super) fn project_mcp_terminal(
    record: &[u8],
    payload: &Value,
    attribution_qualified: bool,
) -> Option<ProjectedMcpTerminal> {
    if payload.get("type").and_then(Value::as_str) != Some("mcp_tool_call_end") {
        return None;
    }
    let expected_call_id = payload.get("call_id").and_then(Value::as_str)?;
    let evidence = serde_json::from_slice::<McpExchangeEnvelope<'_>>(record).ok()?;
    // Preserve the shared exact-JSON member budget as one record-wide bound.
    // The terminal visitor below supplies semantic shape and duplicate-known-key
    // evidence; this non-payload-retaining audit covers nested argument keys and
    // the aggregate 65,536-member ceiling without rebuilding a second Value tree.
    let strict_record_exact = raw_object_keys_are_unique(record);
    let attribution = attribution_qualified
        .then(|| evidence.attribution(expected_call_id))
        .flatten();
    let exchange = evidence.project(payload, expected_call_id, strict_record_exact);
    Some(ProjectedMcpTerminal {
        attribution,
        exchange,
    })
}

pub(super) fn selected_content_fits(
    normalized_body: &str,
    structured_content: Option<&Value>,
    exchange: Option<&ctx_history_core::McpExchangeContent>,
) -> bool {
    jsonl_selected_content_fits(
        normalized_body,
        structured_content,
        exchange,
        ctx_history_core::MAX_CORE_CONTENT_BYTES,
    )
}

#[derive(Default)]
struct McpExchangeEnvelope<'a> {
    record_type: Option<String>,
    payload: Option<McpExchangePayload<'a>>,
    ambiguous: bool,
    strict_invalid: bool,
}

impl McpExchangeEnvelope<'_> {
    fn attribution(
        &self,
        expected_call_id: &str,
    ) -> Option<ctx_history_core::McpToolCallAttribution> {
        if self.ambiguous || self.record_type.as_deref() != Some("event_msg") {
            return None;
        }
        self.payload.as_ref()?.attribution(expected_call_id)
    }

    fn project(
        self,
        decoded_payload: &Value,
        expected_call_id: &str,
        strict_record_exact: bool,
    ) -> Option<ProjectedMcpExchange> {
        if self.ambiguous || self.record_type.as_deref() != Some("event_msg") {
            return None;
        }
        let strict_envelope = strict_record_exact && !self.strict_invalid;
        self.payload?.project(
            decoded_payload,
            expected_call_id,
            strict_envelope,
            strict_record_exact,
        )
    }
}

impl<'de> Deserialize<'de> for McpExchangeEnvelope<'de> {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(McpExchangeEnvelopeVisitor)
    }
}

struct McpExchangeEnvelopeVisitor;

impl<'de> Visitor<'de> for McpExchangeEnvelopeVisitor {
    type Value = McpExchangeEnvelope<'de>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a Codex MCP terminal envelope")
    }

    fn visit_map<M>(self, mut map: M) -> std::result::Result<Self::Value, M::Error>
    where
        M: MapAccess<'de>,
    {
        let mut envelope = McpExchangeEnvelope::default();
        let mut saw_timestamp = false;
        let mut saw_record_type = false;
        let mut saw_payload = false;
        while let Some(key) = map.next_key::<Cow<'de, str>>()? {
            match key.as_ref() {
                "timestamp" => {
                    envelope.strict_invalid |= saw_timestamp;
                    saw_timestamp = true;
                    let timestamp = map.next_value::<&'de RawValue>()?;
                    envelope.strict_invalid |= !serde_json::from_str::<&str>(timestamp.get())
                        .is_ok_and(|timestamp| !timestamp.is_empty());
                }
                "type" => {
                    envelope.ambiguous |= saw_record_type;
                    envelope.strict_invalid |= saw_record_type;
                    saw_record_type = true;
                    envelope.record_type = map.next_value::<BoundedStringProbe<64>>()?.value;
                }
                "payload" => {
                    envelope.ambiguous |= saw_payload;
                    envelope.strict_invalid |= saw_payload;
                    saw_payload = true;
                    envelope.payload = Some(map.next_value::<McpExchangePayload<'de>>()?);
                }
                _ => {
                    envelope.strict_invalid = true;
                    map.next_value::<serde::de::IgnoredAny>()?;
                }
            }
        }
        Ok(envelope)
    }
}

#[derive(Default)]
struct McpExchangePayload<'a> {
    item_type: Option<String>,
    call_id: Option<String>,
    invocation: Option<McpExchangeInvocation<'a>>,
    duration: Option<McpDurationProbe>,
    result: Option<McpResultProbe<'a>>,
    duplicate_item_type: bool,
    duplicate_call_id: bool,
    duplicate_invocation: bool,
    duplicate_duration: bool,
    duplicate_result: bool,
    duplicate_output: bool,
    duplicate_tools: bool,
    strict_invalid: bool,
}

impl McpExchangePayload<'_> {
    fn attribution(
        &self,
        expected_call_id: &str,
    ) -> Option<ctx_history_core::McpToolCallAttribution> {
        if self.duplicate_item_type
            || self.duplicate_call_id
            || self.duplicate_invocation
            || self.duplicate_duration
            || self.duplicate_result
            || self.duplicate_output
            || self.duplicate_tools
            || self.item_type.as_deref() != Some("mcp_tool_call_end")
            || self.call_id.as_deref() != Some(expected_call_id)
        {
            return None;
        }
        self.invocation.as_ref()?.attribution()
    }

    fn project(
        self,
        decoded_payload: &Value,
        expected_call_id: &str,
        strict_envelope: bool,
        record_exact: bool,
    ) -> Option<ProjectedMcpExchange> {
        if self.duplicate_item_type
            || self.duplicate_call_id
            || self.item_type.as_deref() != Some("mcp_tool_call_end")
            || self.call_id.as_deref() != Some(expected_call_id)
        {
            return None;
        }

        let projected_invocation = if self.duplicate_invocation {
            ProjectedMcpInvocation::unavailable()
        } else {
            self.invocation
                .map(|invocation| {
                    invocation.project(decoded_payload.get("invocation"), record_exact)
                })
                .unwrap_or_else(ProjectedMcpInvocation::unavailable)
        };
        let duration_strict = !self.duplicate_duration
            && self.duration.as_ref().is_some_and(McpDurationProbe::strict);
        let duration_ns = (!self.duplicate_duration)
            .then_some(self.duration)
            .flatten()
            .and_then(McpDurationProbe::duration_ns);
        let projected_result = if self.duplicate_result {
            ProjectedMcpResult::unavailable()
        } else {
            self.result
                .and_then(|result| result.project(decoded_payload.get("result"), record_exact))
                .unwrap_or_else(ProjectedMcpResult::unavailable)
        };
        let strict_discovery_payload = strict_envelope
            && !self.strict_invalid
            && projected_invocation.strict
            && duration_strict
            && projected_result.strict;
        let response = ctx_history_core::McpTerminalResponseContent {
            status: projected_result.status,
            failure_kind: projected_result.failure_kind,
            duration_ns,
            text: ctx_history_core::McpTextCapture::NormalizedBody,
            payload: projected_result.payload,
        };
        Some(ProjectedMcpExchange {
            content: ctx_history_core::McpExchangeContent {
                provider_call_id: expected_call_id.to_owned(),
                invocation: projected_invocation.content,
                response: Some(response),
            },
            arguments_observed_encoded_bytes: projected_invocation.observed_encoded_bytes,
            payload_observed_encoded_bytes: projected_result.observed_encoded_bytes,
            strict_discovery_payload,
        })
    }
}

impl<'de> Deserialize<'de> for McpExchangePayload<'de> {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(McpExchangePayloadVisitor)
    }
}

struct McpExchangePayloadVisitor;

impl<'de> Visitor<'de> for McpExchangePayloadVisitor {
    type Value = McpExchangePayload<'de>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a Codex MCP terminal payload")
    }

    fn visit_map<M>(self, mut map: M) -> std::result::Result<Self::Value, M::Error>
    where
        M: MapAccess<'de>,
    {
        let mut payload = McpExchangePayload::default();
        let mut saw_item_type = false;
        let mut saw_call_id = false;
        let mut saw_invocation = false;
        let mut saw_duration = false;
        let mut saw_result = false;
        let mut saw_output = false;
        let mut saw_tools = false;
        while let Some(key) = map.next_key::<Cow<'de, str>>()? {
            match key.as_ref() {
                "type" => {
                    payload.duplicate_item_type |= saw_item_type;
                    payload.strict_invalid |= saw_item_type;
                    saw_item_type = true;
                    payload.item_type = map.next_value::<BoundedStringProbe<64>>()?.value;
                }
                "call_id" => {
                    payload.duplicate_call_id |= saw_call_id;
                    payload.strict_invalid |= saw_call_id;
                    saw_call_id = true;
                    payload.call_id = map
                        .next_value::<BoundedStringProbe<MAX_CODEX_TOOL_CALL_ID_BYTES>>()?
                        .value;
                }
                "invocation" => {
                    payload.duplicate_invocation |= saw_invocation;
                    payload.strict_invalid |= saw_invocation;
                    saw_invocation = true;
                    payload.invocation = Some(map.next_value::<McpExchangeInvocation<'de>>()?);
                }
                "duration" => {
                    payload.duplicate_duration |= saw_duration;
                    payload.strict_invalid |= saw_duration;
                    saw_duration = true;
                    payload.duration = Some(map.next_value()?);
                }
                "result" => {
                    payload.duplicate_result |= saw_result;
                    payload.strict_invalid |= saw_result;
                    saw_result = true;
                    payload.result = Some(map.next_value::<McpResultProbe<'de>>()?);
                }
                "output" => {
                    payload.duplicate_output |= saw_output;
                    payload.strict_invalid = true;
                    saw_output = true;
                    map.next_value::<serde::de::IgnoredAny>()?;
                }
                "tools" => {
                    payload.duplicate_tools |= saw_tools;
                    payload.strict_invalid = true;
                    saw_tools = true;
                    map.next_value::<serde::de::IgnoredAny>()?;
                }
                _ => {
                    payload.strict_invalid = true;
                    map.next_value::<serde::de::IgnoredAny>()?;
                }
            }
        }
        Ok(payload)
    }
}

#[derive(Default)]
struct McpExchangeInvocation<'a> {
    server: Option<String>,
    tool: Option<String>,
    arguments: Option<&'a RawValue>,
    object: bool,
    duplicate_server: bool,
    duplicate_tool: bool,
    duplicate_arguments: bool,
    strict_invalid: bool,
}

impl McpExchangeInvocation<'_> {
    fn attribution(&self) -> Option<ctx_history_core::McpToolCallAttribution> {
        if !self.object || self.duplicate_server || self.duplicate_tool {
            return None;
        }
        let server = self.server.as_ref()?.clone();
        let tool = self.tool.as_ref()?.clone();
        if server.is_empty() || tool.is_empty() {
            return None;
        }
        Some(ctx_history_core::McpToolCallAttribution { server, tool })
    }

    fn project(
        self,
        decoded_invocation: Option<&Value>,
        record_exact: bool,
    ) -> ProjectedMcpInvocation {
        if !self.object || self.duplicate_server || self.duplicate_tool {
            return ProjectedMcpInvocation::unavailable();
        }
        let Some(server) = self.server.filter(|server| !server.is_empty()) else {
            return ProjectedMcpInvocation::unavailable();
        };
        let Some(tool) = self.tool.filter(|tool| !tool.is_empty()) else {
            return ProjectedMcpInvocation::unavailable();
        };
        let decoded_arguments = decoded_invocation
            .and_then(Value::as_object)
            .and_then(|invocation| invocation.get("arguments"));
        let (arguments, observed_encoded_bytes, arguments_strict) = if self.duplicate_arguments {
            (ctx_history_core::McpJsonCapture::Unavailable, None, false)
        } else if let Some(raw) = self.arguments {
            let observed = u64::try_from(raw.get().len()).ok();
            let exact = record_exact || raw_object_keys_are_unique(raw.get().as_bytes());
            let capture = exact
                .then_some(decoded_arguments)
                .flatten()
                .filter(|value| value.is_object())
                .cloned()
                .map(|value| ctx_history_core::McpJsonCapture::Present { value })
                .unwrap_or(ctx_history_core::McpJsonCapture::Unavailable);
            (capture, observed, exact)
        } else {
            (ctx_history_core::McpJsonCapture::Absent, None, true)
        };
        ProjectedMcpInvocation {
            content: Some(ctx_history_core::McpInvocationContent {
                server,
                tool,
                arguments,
            }),
            observed_encoded_bytes,
            strict: !self.strict_invalid && arguments_strict,
        }
    }
}

struct ProjectedMcpInvocation {
    content: Option<ctx_history_core::McpInvocationContent>,
    observed_encoded_bytes: Option<u64>,
    strict: bool,
}

impl ProjectedMcpInvocation {
    fn unavailable() -> Self {
        Self {
            content: None,
            observed_encoded_bytes: None,
            strict: false,
        }
    }
}

impl<'de> Deserialize<'de> for McpExchangeInvocation<'de> {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(McpExchangeInvocationVisitor)
    }
}

struct McpExchangeInvocationVisitor;

impl<'de> Visitor<'de> for McpExchangeInvocationVisitor {
    type Value = McpExchangeInvocation<'de>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a Codex MCP invocation object")
    }

    fn visit_map<M>(self, mut map: M) -> std::result::Result<Self::Value, M::Error>
    where
        M: MapAccess<'de>,
    {
        let mut invocation = McpExchangeInvocation {
            object: true,
            ..McpExchangeInvocation::default()
        };
        let mut saw_server = false;
        let mut saw_tool = false;
        let mut saw_arguments = false;
        while let Some(key) = map.next_key::<Cow<'de, str>>()? {
            match key.as_ref() {
                "server" => {
                    invocation.duplicate_server |= saw_server;
                    invocation.strict_invalid |= saw_server;
                    saw_server = true;
                    invocation.server = map
                        .next_value::<BoundedStringProbe<
                            { ctx_history_core::MAX_MCP_TOOL_CALL_ATTRIBUTION_COMPONENT_BYTES },
                        >>()?
                        .value;
                }
                "tool" => {
                    invocation.duplicate_tool |= saw_tool;
                    invocation.strict_invalid |= saw_tool;
                    saw_tool = true;
                    invocation.tool = map
                        .next_value::<BoundedStringProbe<
                            { ctx_history_core::MAX_MCP_TOOL_CALL_ATTRIBUTION_COMPONENT_BYTES },
                        >>()?
                        .value;
                }
                "arguments" => {
                    invocation.duplicate_arguments |= saw_arguments;
                    invocation.strict_invalid |= saw_arguments;
                    saw_arguments = true;
                    invocation.arguments = Some(map.next_value::<&'de RawValue>()?);
                }
                _ => {
                    invocation.strict_invalid = true;
                    map.next_value::<serde::de::IgnoredAny>()?;
                }
            }
        }
        Ok(invocation)
    }

    fn visit_bool<E>(self, _value: bool) -> std::result::Result<Self::Value, E> {
        Ok(McpExchangeInvocation::default())
    }

    fn visit_i64<E>(self, _value: i64) -> std::result::Result<Self::Value, E> {
        Ok(McpExchangeInvocation::default())
    }

    fn visit_u64<E>(self, _value: u64) -> std::result::Result<Self::Value, E> {
        Ok(McpExchangeInvocation::default())
    }

    fn visit_f64<E>(self, _value: f64) -> std::result::Result<Self::Value, E> {
        Ok(McpExchangeInvocation::default())
    }

    fn visit_borrowed_str<E>(self, _value: &'de str) -> std::result::Result<Self::Value, E> {
        Ok(McpExchangeInvocation::default())
    }

    fn visit_str<E>(self, _value: &str) -> std::result::Result<Self::Value, E> {
        Ok(McpExchangeInvocation::default())
    }

    fn visit_string<E>(self, _value: String) -> std::result::Result<Self::Value, E> {
        Ok(McpExchangeInvocation::default())
    }

    fn visit_none<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(McpExchangeInvocation::default())
    }

    fn visit_unit<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(McpExchangeInvocation::default())
    }

    fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while sequence.next_element::<serde::de::IgnoredAny>()?.is_some() {}
        Ok(McpExchangeInvocation::default())
    }
}

#[derive(Default)]
struct McpDurationProbe {
    secs: Option<u64>,
    nanos: Option<u64>,
    object: bool,
    ambiguous: bool,
    strict_invalid: bool,
}

impl McpDurationProbe {
    fn strict(&self) -> bool {
        self.object
            && !self.ambiguous
            && !self.strict_invalid
            && self.secs.is_some()
            && self.nanos.is_some_and(|nanos| nanos < 1_000_000_000)
    }

    fn duration_ns(self) -> Option<u64> {
        if !self.object || self.ambiguous || self.nanos? >= 1_000_000_000 {
            return None;
        }
        self.secs?
            .checked_mul(1_000_000_000)?
            .checked_add(self.nanos?)
    }
}

impl<'de> Deserialize<'de> for McpDurationProbe {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(McpDurationProbeVisitor)
    }
}

struct McpDurationProbeVisitor;

impl<'de> Visitor<'de> for McpDurationProbeVisitor {
    type Value = McpDurationProbe;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a Codex MCP duration object")
    }

    fn visit_map<M>(self, mut map: M) -> std::result::Result<Self::Value, M::Error>
    where
        M: MapAccess<'de>,
    {
        let mut duration = McpDurationProbe {
            object: true,
            ..McpDurationProbe::default()
        };
        let mut saw_secs = false;
        let mut saw_nanos = false;
        while let Some(key) = map.next_key::<Cow<'de, str>>()? {
            match key.as_ref() {
                "secs" => {
                    duration.ambiguous |= saw_secs;
                    saw_secs = true;
                    duration.secs = map.next_value::<U64Probe>()?.0;
                }
                "nanos" => {
                    duration.ambiguous |= saw_nanos;
                    saw_nanos = true;
                    duration.nanos = map.next_value::<U64Probe>()?.0;
                }
                _ => {
                    duration.strict_invalid = true;
                    map.next_value::<serde::de::IgnoredAny>()?;
                }
            }
        }
        Ok(duration)
    }

    fn visit_bool<E>(self, _value: bool) -> std::result::Result<Self::Value, E> {
        Ok(McpDurationProbe::default())
    }

    fn visit_i64<E>(self, _value: i64) -> std::result::Result<Self::Value, E> {
        Ok(McpDurationProbe::default())
    }

    fn visit_u64<E>(self, _value: u64) -> std::result::Result<Self::Value, E> {
        Ok(McpDurationProbe::default())
    }

    fn visit_f64<E>(self, _value: f64) -> std::result::Result<Self::Value, E> {
        Ok(McpDurationProbe::default())
    }

    fn visit_borrowed_str<E>(self, _value: &'de str) -> std::result::Result<Self::Value, E> {
        Ok(McpDurationProbe::default())
    }

    fn visit_str<E>(self, _value: &str) -> std::result::Result<Self::Value, E> {
        Ok(McpDurationProbe::default())
    }

    fn visit_string<E>(self, _value: String) -> std::result::Result<Self::Value, E> {
        Ok(McpDurationProbe::default())
    }

    fn visit_none<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(McpDurationProbe::default())
    }

    fn visit_unit<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(McpDurationProbe::default())
    }

    fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while sequence.next_element::<serde::de::IgnoredAny>()?.is_some() {}
        Ok(McpDurationProbe::default())
    }
}

struct U64Probe(Option<u64>);

impl<'de> Deserialize<'de> for U64Probe {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(U64ProbeVisitor)
    }
}

struct U64ProbeVisitor;

impl<'de> Visitor<'de> for U64ProbeVisitor {
    type Value = U64Probe;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON unsigned integer")
    }

    fn visit_u64<E>(self, value: u64) -> std::result::Result<Self::Value, E> {
        Ok(U64Probe(Some(value)))
    }

    fn visit_i64<E>(self, _value: i64) -> std::result::Result<Self::Value, E> {
        Ok(U64Probe(None))
    }

    fn visit_f64<E>(self, _value: f64) -> std::result::Result<Self::Value, E> {
        Ok(U64Probe(None))
    }

    fn visit_bool<E>(self, _value: bool) -> std::result::Result<Self::Value, E> {
        Ok(U64Probe(None))
    }

    fn visit_borrowed_str<E>(self, _value: &'de str) -> std::result::Result<Self::Value, E> {
        Ok(U64Probe(None))
    }

    fn visit_str<E>(self, _value: &str) -> std::result::Result<Self::Value, E> {
        Ok(U64Probe(None))
    }

    fn visit_string<E>(self, _value: String) -> std::result::Result<Self::Value, E> {
        Ok(U64Probe(None))
    }

    fn visit_none<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(U64Probe(None))
    }

    fn visit_unit<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(U64Probe(None))
    }

    fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while sequence.next_element::<serde::de::IgnoredAny>()?.is_some() {}
        Ok(U64Probe(None))
    }

    fn visit_map<M>(self, mut map: M) -> std::result::Result<Self::Value, M::Error>
    where
        M: MapAccess<'de>,
    {
        while map
            .next_entry::<serde::de::IgnoredAny, serde::de::IgnoredAny>()?
            .is_some()
        {}
        Ok(U64Probe(None))
    }
}

#[derive(Clone, Copy)]
enum McpResultVariant {
    Ok,
    Err,
}

#[derive(Default)]
struct McpResultProbe<'a> {
    selected: Option<(McpResultVariant, &'a RawValue)>,
    object: bool,
    members: usize,
}

impl McpResultProbe<'_> {
    fn project(
        self,
        decoded_result: Option<&Value>,
        record_exact: bool,
    ) -> Option<ProjectedMcpResult> {
        if !self.object || self.members != 1 {
            return None;
        }
        let (variant, raw) = self.selected?;
        let variant_name = match variant {
            McpResultVariant::Ok => "Ok",
            McpResultVariant::Err => "Err",
        };
        let decoded = decoded_result
            .and_then(Value::as_object)
            .and_then(|result| result.get(variant_name))?;
        let observed_encoded_bytes = u64::try_from(raw.get().len()).ok();
        let exact = record_exact || raw_object_keys_are_unique(raw.get().as_bytes());
        let payload = if exact {
            ctx_history_core::McpJsonCapture::Present {
                value: decoded.clone(),
            }
        } else {
            ctx_history_core::McpJsonCapture::Unavailable
        };
        let (status, failure_kind) = match variant {
            McpResultVariant::Err => (
                ctx_history_core::McpTerminalStatus::Failed,
                Some(ctx_history_core::McpFailureKind::Invocation),
            ),
            McpResultVariant::Ok => match if exact {
                decoded_ok_is_error(decoded)
            } else {
                exact_ok_is_error(raw.get())
            } {
                Some(true) => (
                    ctx_history_core::McpTerminalStatus::Failed,
                    Some(ctx_history_core::McpFailureKind::ToolReported),
                ),
                Some(false) => (ctx_history_core::McpTerminalStatus::Succeeded, None),
                None => (ctx_history_core::McpTerminalStatus::Unknown, None),
            },
        };
        Some(ProjectedMcpResult {
            status,
            failure_kind,
            payload,
            observed_encoded_bytes,
            strict: matches!(variant, McpResultVariant::Ok)
                && exact
                && strict_mcp_retrieval_ok(decoded),
        })
    }
}

impl<'de> Deserialize<'de> for McpResultProbe<'de> {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(McpResultProbeVisitor)
    }
}

struct McpResultProbeVisitor;

impl<'de> Visitor<'de> for McpResultProbeVisitor {
    type Value = McpResultProbe<'de>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a Codex MCP Ok/Err result wrapper")
    }

    fn visit_map<M>(self, mut map: M) -> std::result::Result<Self::Value, M::Error>
    where
        M: MapAccess<'de>,
    {
        let mut result = McpResultProbe {
            object: true,
            ..McpResultProbe::default()
        };
        while let Some(key) = map.next_key::<Cow<'de, str>>()? {
            let raw = map.next_value::<&'de RawValue>()?;
            result.members = result.members.saturating_add(1);
            if result.members == 1 {
                result.selected = match key.as_ref() {
                    "Ok" => Some((McpResultVariant::Ok, raw)),
                    "Err" => Some((McpResultVariant::Err, raw)),
                    _ => None,
                };
            }
        }
        Ok(result)
    }

    fn visit_bool<E>(self, _value: bool) -> std::result::Result<Self::Value, E> {
        Ok(McpResultProbe::default())
    }

    fn visit_i64<E>(self, _value: i64) -> std::result::Result<Self::Value, E> {
        Ok(McpResultProbe::default())
    }

    fn visit_u64<E>(self, _value: u64) -> std::result::Result<Self::Value, E> {
        Ok(McpResultProbe::default())
    }

    fn visit_f64<E>(self, _value: f64) -> std::result::Result<Self::Value, E> {
        Ok(McpResultProbe::default())
    }

    fn visit_borrowed_str<E>(self, _value: &'de str) -> std::result::Result<Self::Value, E> {
        Ok(McpResultProbe::default())
    }

    fn visit_str<E>(self, _value: &str) -> std::result::Result<Self::Value, E> {
        Ok(McpResultProbe::default())
    }

    fn visit_string<E>(self, _value: String) -> std::result::Result<Self::Value, E> {
        Ok(McpResultProbe::default())
    }

    fn visit_none<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(McpResultProbe::default())
    }

    fn visit_unit<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(McpResultProbe::default())
    }

    fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while sequence.next_element::<serde::de::IgnoredAny>()?.is_some() {}
        Ok(McpResultProbe::default())
    }
}

struct ProjectedMcpResult {
    status: ctx_history_core::McpTerminalStatus,
    failure_kind: Option<ctx_history_core::McpFailureKind>,
    payload: ctx_history_core::McpJsonCapture,
    observed_encoded_bytes: Option<u64>,
    strict: bool,
}

impl ProjectedMcpResult {
    fn unavailable() -> Self {
        Self {
            status: ctx_history_core::McpTerminalStatus::Unknown,
            failure_kind: None,
            payload: ctx_history_core::McpJsonCapture::Unavailable,
            observed_encoded_bytes: None,
            strict: false,
        }
    }
}

fn decoded_ok_is_error(value: &Value) -> Option<bool> {
    let object = value.as_object()?;
    match object.get("isError") {
        Some(value) => value.as_bool(),
        None => Some(false),
    }
}

fn strict_mcp_retrieval_ok(value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    if !object
        .keys()
        .all(|key| matches!(key.as_str(), "content" | "isError"))
        || object
            .get("isError")
            .is_some_and(|is_error| is_error.as_bool() != Some(false))
    {
        return false;
    }
    let Some(content) = object.get("content").and_then(Value::as_array) else {
        return false;
    };
    let mut saw_payload = false;
    for block in content {
        let Some(block) = block.as_object() else {
            return false;
        };
        if !block
            .keys()
            .all(|key| matches!(key.as_str(), "type" | "text"))
            || block.get("type").and_then(Value::as_str) != Some("text")
        {
            return false;
        }
        let Some(text) = block.get("text").and_then(Value::as_str) else {
            return false;
        };
        saw_payload |= !text.is_empty();
    }
    saw_payload
}

fn exact_ok_is_error(input: &str) -> Option<bool> {
    let probe = serde_json::from_str::<McpOkErrorProbe>(input).ok()?;
    if !probe.object || probe.ambiguous {
        return None;
    }
    if probe.saw_is_error {
        probe.is_error
    } else {
        Some(false)
    }
}

#[derive(Default)]
struct McpOkErrorProbe {
    is_error: Option<bool>,
    saw_is_error: bool,
    object: bool,
    ambiguous: bool,
}

impl<'de> Deserialize<'de> for McpOkErrorProbe {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(McpOkErrorProbeVisitor)
    }
}

struct McpOkErrorProbeVisitor;

impl<'de> Visitor<'de> for McpOkErrorProbeVisitor {
    type Value = McpOkErrorProbe;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a Codex MCP Ok result")
    }

    fn visit_map<M>(self, mut map: M) -> std::result::Result<Self::Value, M::Error>
    where
        M: MapAccess<'de>,
    {
        let mut probe = McpOkErrorProbe {
            object: true,
            ..McpOkErrorProbe::default()
        };
        while let Some(key) = map.next_key::<Cow<'de, str>>()? {
            if key == "isError" {
                probe.ambiguous |= probe.saw_is_error;
                probe.saw_is_error = true;
                probe.is_error = map.next_value::<BoolProbe>()?.0;
            } else {
                map.next_value::<serde::de::IgnoredAny>()?;
            }
        }
        Ok(probe)
    }

    fn visit_bool<E>(self, _value: bool) -> std::result::Result<Self::Value, E> {
        Ok(McpOkErrorProbe::default())
    }

    fn visit_i64<E>(self, _value: i64) -> std::result::Result<Self::Value, E> {
        Ok(McpOkErrorProbe::default())
    }

    fn visit_u64<E>(self, _value: u64) -> std::result::Result<Self::Value, E> {
        Ok(McpOkErrorProbe::default())
    }

    fn visit_f64<E>(self, _value: f64) -> std::result::Result<Self::Value, E> {
        Ok(McpOkErrorProbe::default())
    }

    fn visit_borrowed_str<E>(self, _value: &'de str) -> std::result::Result<Self::Value, E> {
        Ok(McpOkErrorProbe::default())
    }

    fn visit_str<E>(self, _value: &str) -> std::result::Result<Self::Value, E> {
        Ok(McpOkErrorProbe::default())
    }

    fn visit_string<E>(self, _value: String) -> std::result::Result<Self::Value, E> {
        Ok(McpOkErrorProbe::default())
    }

    fn visit_none<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(McpOkErrorProbe::default())
    }

    fn visit_unit<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(McpOkErrorProbe::default())
    }

    fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while sequence.next_element::<serde::de::IgnoredAny>()?.is_some() {}
        Ok(McpOkErrorProbe::default())
    }
}

struct BoolProbe(Option<bool>);

impl<'de> Deserialize<'de> for BoolProbe {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(BoolProbeVisitor)
    }
}

struct BoolProbeVisitor;

impl<'de> Visitor<'de> for BoolProbeVisitor {
    type Value = BoolProbe;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON boolean")
    }

    fn visit_bool<E>(self, value: bool) -> std::result::Result<Self::Value, E> {
        Ok(BoolProbe(Some(value)))
    }

    fn visit_i64<E>(self, _value: i64) -> std::result::Result<Self::Value, E> {
        Ok(BoolProbe(None))
    }

    fn visit_u64<E>(self, _value: u64) -> std::result::Result<Self::Value, E> {
        Ok(BoolProbe(None))
    }

    fn visit_f64<E>(self, _value: f64) -> std::result::Result<Self::Value, E> {
        Ok(BoolProbe(None))
    }

    fn visit_borrowed_str<E>(self, _value: &'de str) -> std::result::Result<Self::Value, E> {
        Ok(BoolProbe(None))
    }

    fn visit_str<E>(self, _value: &str) -> std::result::Result<Self::Value, E> {
        Ok(BoolProbe(None))
    }

    fn visit_string<E>(self, _value: String) -> std::result::Result<Self::Value, E> {
        Ok(BoolProbe(None))
    }

    fn visit_none<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(BoolProbe(None))
    }

    fn visit_unit<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(BoolProbe(None))
    }

    fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while sequence.next_element::<serde::de::IgnoredAny>()?.is_some() {}
        Ok(BoolProbe(None))
    }

    fn visit_map<M>(self, mut map: M) -> std::result::Result<Self::Value, M::Error>
    where
        M: MapAccess<'de>,
    {
        while map
            .next_entry::<serde::de::IgnoredAny, serde::de::IgnoredAny>()?
            .is_some()
        {}
        Ok(BoolProbe(None))
    }
}

#[cfg(test)]
mod tests {
    use super::exact_ok_is_error;

    #[test]
    fn ok_is_error_absence_defaults_to_success_but_invalid_or_duplicate_is_unknown() {
        assert_eq!(exact_ok_is_error(r#"{"content":[]}"#), Some(false));
        assert_eq!(exact_ok_is_error(r#"{"isError":false}"#), Some(false));
        assert_eq!(exact_ok_is_error(r#"{"isError":true}"#), Some(true));
        assert_eq!(exact_ok_is_error(r#"{"isError":"false"}"#), None);
        assert_eq!(exact_ok_is_error(r#"{"isError":null}"#), None);
        assert_eq!(
            exact_ok_is_error(r#"{"isError":false,"isError":true}"#),
            None
        );
    }
}
