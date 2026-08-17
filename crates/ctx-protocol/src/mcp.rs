use super::*;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpToolCall {
    #[serde(
        deserialize_with = "deserialize_mcp_tool_call_component",
        serialize_with = "serialize_mcp_tool_call_component"
    )]
    pub server: String,
    #[serde(
        deserialize_with = "deserialize_mcp_tool_call_component",
        serialize_with = "serialize_mcp_tool_call_component"
    )]
    pub tool: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpExchange {
    pub provider_call_id: String,
    pub invocation: Option<McpInvocation>,
    pub response: Option<McpResponse>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpInvocation {
    pub server: String,
    pub tool: String,
    pub arguments: McpJsonCapture,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpResponse {
    pub status: McpResponseStatus,
    pub failure_kind: Option<McpFailureKind>,
    pub duration_ns: Option<u64>,
    pub text: McpTextCapture,
    pub payload: McpJsonCapture,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpJsonCapture {
    Present {
        value: Value,
    },
    Absent,
    Unavailable,
    Omitted {
        reason: McpPayloadOmissionReason,
        observed_encoded_bytes: Option<u64>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpTextCapture {
    NormalizedBody,
    Absent,
    Unavailable,
    Omitted {
        reason: McpPayloadOmissionReason,
        observed_encoded_bytes: Option<u64>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpPayloadOmissionReason {
    SizeLimit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpResponseStatus {
    Succeeded,
    Failed,
    Cancelled,
    TimedOut,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpFailureKind {
    ToolReported,
    Invocation,
    Unknown,
}

impl<'de> Deserialize<'de> for McpJsonCapture {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        parse_mcp_json_capture(ExactJsonValue::deserialize(deserializer)?.0)
            .map_err(serde::de::Error::custom)
    }
}

impl Serialize for McpJsonCapture {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        validate_mcp_json_capture(self).map_err(serde::ser::Error::custom)?;
        mcp_json_capture_value(self).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for McpTextCapture {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        parse_mcp_text_capture(ExactJsonValue::deserialize(deserializer)?.0)
            .map_err(serde::de::Error::custom)
    }
}

impl Serialize for McpTextCapture {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        validate_mcp_text_capture(self).map_err(serde::ser::Error::custom)?;
        mcp_text_capture_value(self).serialize(serializer)
    }
}

fn parse_mcp_json_capture(value: Value) -> Result<McpJsonCapture, String> {
    let mut object = mcp_capture_object(value)?;
    let status = take_capture_status(&mut object)?;
    let capture = match status.as_str() {
        "present" => McpJsonCapture::Present {
            value: object
                .remove("value")
                .ok_or_else(|| "present MCP JSON capture requires value".to_owned())?,
        },
        "absent" => McpJsonCapture::Absent,
        "unavailable" => McpJsonCapture::Unavailable,
        "omitted" => {
            let (reason, observed_encoded_bytes) = take_omission_fields(&mut object)?;
            McpJsonCapture::Omitted {
                reason,
                observed_encoded_bytes,
            }
        }
        _ => return Err(format!("unknown MCP JSON captureStatus {status:?}")),
    };
    reject_remaining_capture_fields(&object)?;
    Ok(capture)
}

fn parse_mcp_text_capture(value: Value) -> Result<McpTextCapture, String> {
    let mut object = mcp_capture_object(value)?;
    let status = take_capture_status(&mut object)?;
    let capture = match status.as_str() {
        "normalized_body" => McpTextCapture::NormalizedBody,
        "absent" => McpTextCapture::Absent,
        "unavailable" => McpTextCapture::Unavailable,
        "omitted" => {
            let (reason, observed_encoded_bytes) = take_omission_fields(&mut object)?;
            McpTextCapture::Omitted {
                reason,
                observed_encoded_bytes,
            }
        }
        _ => return Err(format!("unknown MCP text captureStatus {status:?}")),
    };
    reject_remaining_capture_fields(&object)?;
    Ok(capture)
}

fn mcp_capture_object(value: Value) -> Result<Map<String, Value>, String> {
    match value {
        Value::Object(object) => Ok(object),
        _ => Err("MCP capture must be an object".to_owned()),
    }
}

fn take_capture_status(object: &mut Map<String, Value>) -> Result<String, String> {
    object
        .remove("captureStatus")
        .and_then(|value| value.as_str().map(str::to_owned))
        .ok_or_else(|| "MCP captureStatus must be a string".to_owned())
}

fn take_omission_fields(
    object: &mut Map<String, Value>,
) -> Result<(McpPayloadOmissionReason, Option<u64>), String> {
    let reason = match object
        .remove("reason")
        .and_then(|value| value.as_str().map(str::to_owned))
    {
        Some(reason) if reason == "size_limit" => McpPayloadOmissionReason::SizeLimit,
        Some(reason) => return Err(format!("unknown MCP capture omission reason {reason:?}")),
        None => return Err("omitted MCP capture requires string reason".to_owned()),
    };
    let observed_encoded_bytes = object
        .remove("observedEncodedBytes")
        .map(|value| {
            value
                .as_u64()
                .ok_or_else(|| "observedEncodedBytes must be an unsigned integer".to_owned())
        })
        .transpose()?;
    validate_optional_safe_integer("observedEncodedBytes", observed_encoded_bytes)?;
    Ok((reason, observed_encoded_bytes))
}

fn validate_mcp_json_capture(capture: &McpJsonCapture) -> Result<(), String> {
    if let McpJsonCapture::Omitted {
        observed_encoded_bytes,
        ..
    } = capture
    {
        validate_optional_safe_integer("observedEncodedBytes", *observed_encoded_bytes)?;
    }
    Ok(())
}

fn validate_mcp_text_capture(capture: &McpTextCapture) -> Result<(), String> {
    if let McpTextCapture::Omitted {
        observed_encoded_bytes,
        ..
    } = capture
    {
        validate_optional_safe_integer("observedEncodedBytes", *observed_encoded_bytes)?;
    }
    Ok(())
}

fn reject_remaining_capture_fields(object: &Map<String, Value>) -> Result<(), String> {
    match object.keys().next() {
        Some(key) => Err(format!("MCP capture contains unknown member {key:?}")),
        None => Ok(()),
    }
}

fn mcp_json_capture_value(capture: &McpJsonCapture) -> Value {
    let mut object = Map::new();
    match capture {
        McpJsonCapture::Present { value } => {
            object.insert(
                "captureStatus".to_owned(),
                Value::String("present".to_owned()),
            );
            object.insert("value".to_owned(), value.clone());
        }
        McpJsonCapture::Absent => {
            object.insert(
                "captureStatus".to_owned(),
                Value::String("absent".to_owned()),
            );
        }
        McpJsonCapture::Unavailable => {
            object.insert(
                "captureStatus".to_owned(),
                Value::String("unavailable".to_owned()),
            );
        }
        McpJsonCapture::Omitted {
            reason,
            observed_encoded_bytes,
        } => insert_omitted_capture_fields(&mut object, *reason, *observed_encoded_bytes),
    }
    Value::Object(object)
}

fn mcp_text_capture_value(capture: &McpTextCapture) -> Value {
    let mut object = Map::new();
    match capture {
        McpTextCapture::NormalizedBody => {
            object.insert(
                "captureStatus".to_owned(),
                Value::String("normalized_body".to_owned()),
            );
        }
        McpTextCapture::Absent => {
            object.insert(
                "captureStatus".to_owned(),
                Value::String("absent".to_owned()),
            );
        }
        McpTextCapture::Unavailable => {
            object.insert(
                "captureStatus".to_owned(),
                Value::String("unavailable".to_owned()),
            );
        }
        McpTextCapture::Omitted {
            reason,
            observed_encoded_bytes,
        } => insert_omitted_capture_fields(&mut object, *reason, *observed_encoded_bytes),
    }
    Value::Object(object)
}

fn insert_omitted_capture_fields(
    object: &mut Map<String, Value>,
    reason: McpPayloadOmissionReason,
    observed_encoded_bytes: Option<u64>,
) {
    object.insert(
        "captureStatus".to_owned(),
        Value::String("omitted".to_owned()),
    );
    let reason = match reason {
        McpPayloadOmissionReason::SizeLimit => "size_limit",
    };
    object.insert("reason".to_owned(), Value::String(reason.to_owned()));
    if let Some(observed_encoded_bytes) = observed_encoded_bytes {
        object.insert(
            "observedEncodedBytes".to_owned(),
            Value::Number(observed_encoded_bytes.into()),
        );
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct McpExchangeWire {
    provider_call_id: String,
    #[serde(default, deserialize_with = "deserialize_present_option")]
    invocation: Option<McpInvocation>,
    #[serde(default, deserialize_with = "deserialize_present_option")]
    response: Option<McpResponse>,
}

impl McpExchange {
    fn validate(&self) -> Result<(), String> {
        validate_mcp_exchange_identity("MCP exchange providerCallId", &self.provider_call_id)?;
        if self.invocation.is_none() && self.response.is_none() {
            return Err("MCP exchange requires invocation, response, or both".to_owned());
        }
        if let Some(invocation) = &self.invocation {
            invocation.validate()?;
        }
        if let Some(response) = &self.response {
            response.validate()?;
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for McpExchange {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = McpExchangeWire::deserialize(deserializer)?;
        let value = Self {
            provider_call_id: wire.provider_call_id,
            invocation: wire.invocation,
            response: wire.response,
        };
        value.validate().map_err(serde::de::Error::custom)?;
        Ok(value)
    }
}

impl Serialize for McpExchange {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.validate().map_err(serde::ser::Error::custom)?;
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Wire<'a> {
            provider_call_id: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            invocation: &'a Option<McpInvocation>,
            #[serde(skip_serializing_if = "Option::is_none")]
            response: &'a Option<McpResponse>,
        }
        Wire {
            provider_call_id: &self.provider_call_id,
            invocation: &self.invocation,
            response: &self.response,
        }
        .serialize(serializer)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct McpInvocationWire {
    server: String,
    tool: String,
    arguments: McpJsonCapture,
}

impl McpInvocation {
    fn validate(&self) -> Result<(), String> {
        validate_mcp_exchange_identity("MCP invocation server", &self.server)?;
        validate_mcp_exchange_identity("MCP invocation tool", &self.tool)?;
        if matches!(
            &self.arguments,
            McpJsonCapture::Present { value } if !value.is_object()
        ) {
            return Err("present MCP invocation arguments must be a JSON object".to_owned());
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for McpInvocation {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = McpInvocationWire::deserialize(deserializer)?;
        let value = Self {
            server: wire.server,
            tool: wire.tool,
            arguments: wire.arguments,
        };
        value.validate().map_err(serde::de::Error::custom)?;
        Ok(value)
    }
}

impl Serialize for McpInvocation {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.validate().map_err(serde::ser::Error::custom)?;
        #[derive(Serialize)]
        struct Wire<'a> {
            server: &'a str,
            tool: &'a str,
            arguments: &'a McpJsonCapture,
        }
        Wire {
            server: &self.server,
            tool: &self.tool,
            arguments: &self.arguments,
        }
        .serialize(serializer)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct McpResponseWire {
    status: McpResponseStatus,
    #[serde(default, deserialize_with = "deserialize_present_option")]
    failure_kind: Option<McpFailureKind>,
    #[serde(default, deserialize_with = "deserialize_present_option")]
    duration_ns: Option<u64>,
    text: McpTextCapture,
    payload: McpJsonCapture,
}

impl McpResponse {
    fn validate(&self) -> Result<(), String> {
        if (self.status == McpResponseStatus::Failed) != self.failure_kind.is_some() {
            return Err(
                "MCP response failureKind must be present exactly when status is failed".to_owned(),
            );
        }
        validate_optional_safe_integer("MCP response durationNs", self.duration_ns)?;
        validate_mcp_text_capture(&self.text)?;
        validate_mcp_json_capture(&self.payload)?;
        Ok(())
    }
}

impl<'de> Deserialize<'de> for McpResponse {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = McpResponseWire::deserialize(deserializer)?;
        let value = Self {
            status: wire.status,
            failure_kind: wire.failure_kind,
            duration_ns: wire.duration_ns,
            text: wire.text,
            payload: wire.payload,
        };
        value.validate().map_err(serde::de::Error::custom)?;
        Ok(value)
    }
}

impl Serialize for McpResponse {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.validate().map_err(serde::ser::Error::custom)?;
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Wire<'a> {
            status: McpResponseStatus,
            #[serde(skip_serializing_if = "Option::is_none")]
            failure_kind: &'a Option<McpFailureKind>,
            #[serde(skip_serializing_if = "Option::is_none")]
            duration_ns: &'a Option<u64>,
            text: &'a McpTextCapture,
            payload: &'a McpJsonCapture,
        }
        Wire {
            status: self.status,
            failure_kind: &self.failure_kind,
            duration_ns: &self.duration_ns,
            text: &self.text,
            payload: &self.payload,
        }
        .serialize(serializer)
    }
}

