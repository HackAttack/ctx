use ctx_history_core::{EventRole, EventType};
use serde_json::Value;

pub fn event_type(value: &Value) -> EventType {
    match value.get("type").and_then(Value::as_str) {
        Some("user_input" | "planner_response") => EventType::Message,
        Some("code_action") => EventType::ToolCall,
        Some("summary" | "checkpoint") => EventType::Summary,
        _ => EventType::Notice,
    }
}

pub fn event_role(value: &Value) -> EventRole {
    match value.get("type").and_then(Value::as_str) {
        Some("user_input") => EventRole::User,
        Some("planner_response") => EventRole::Assistant,
        Some("code_action") => EventRole::Tool,
        _ => EventRole::Unknown,
    }
}

pub fn event_text(value: &Value, entry_type: &str) -> String {
    match entry_type {
        "user_input" => value
            .pointer("/user_input/user_response")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| value.get("user_input").and_then(windsurf_extract_text))
            .unwrap_or_default(),
        "planner_response" => value
            .pointer("/planner_response/response")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| {
                value
                    .get("planner_response")
                    .and_then(windsurf_extract_text)
            })
            .unwrap_or_default(),
        "code_action" => value
            .pointer("/code_action/path")
            .and_then(Value::as_str)
            .filter(|path| !path.trim().is_empty())
            .map(|path| format!("Windsurf code action: {path}"))
            .unwrap_or_default(),
        _ => windsurf_extract_text(value)
            .filter(|text| !text.trim().is_empty())
            .unwrap_or_default(),
    }
}

fn windsurf_extract_text(value: &Value) -> Option<String> {
    let mut parts = Vec::new();
    windsurf_collect_text(value, None, &mut parts);
    (!parts.is_empty()).then(|| parts.join("\n"))
}

fn windsurf_collect_text(value: &Value, key: Option<&str>, out: &mut Vec<String>) {
    match value {
        Value::String(text) => {
            if !windsurf_large_content_key(key.unwrap_or_default()) && !text.trim().is_empty() {
                let label =
                    key.filter(|key| !matches!(normalized_key(key).as_str(), "text" | "message"));
                if let Some(label) = label {
                    out.push(format!("{label}: {text}"));
                } else {
                    out.push(text.to_owned());
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                windsurf_collect_text(item, key, out);
            }
        }
        Value::Object(object) => {
            for wanted in [
                "user_response",
                "response",
                "text",
                "message",
                "summary",
                "path",
                "tool",
                "name",
                "status",
                "type",
            ] {
                if let Some(child) = object.get(wanted) {
                    windsurf_collect_text(child, Some(wanted), out);
                }
            }
            for (child_key, child) in object {
                if matches!(
                    normalized_key(child_key).as_str(),
                    "userresponse"
                        | "response"
                        | "text"
                        | "message"
                        | "summary"
                        | "path"
                        | "tool"
                        | "name"
                        | "status"
                        | "type"
                ) {
                    continue;
                }
                windsurf_collect_text(child, Some(child_key), out);
            }
        }
        Value::Number(_) | Value::Bool(_)
            if !windsurf_large_content_key(key.unwrap_or_default()) =>
        {
            if let Some(key) = key {
                out.push(format!("{key}: {value}"));
            }
        }
        Value::Number(_) | Value::Bool(_) | Value::Null => {}
    }
}

fn windsurf_large_content_key(key: &str) -> bool {
    matches!(
        normalized_key(key).as_str(),
        "newcontent"
            | "oldcontent"
            | "filecontent"
            | "filecontents"
            | "content"
            | "output"
            | "stdout"
            | "stderr"
            | "commandoutput"
            | "toolarguments"
            | "arguments"
            | "args"
            | "result"
            | "results"
            | "searchresults"
    )
}

fn normalized_key(key: &str) -> String {
    key.chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}
