use serde_json::Value;

pub(crate) fn codex_content_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Array(blocks) => {
            let mut parts = Vec::new();
            for block in blocks {
                if let Some(text) = block
                    .get("text")
                    .or_else(|| block.get("input_text"))
                    .or_else(|| block.get("output_text"))
                    .or_else(|| block.get("summary_text"))
                    .and_then(Value::as_str)
                {
                    parts.push(text.to_owned());
                    continue;
                }
                if let Some(text) = block.get("content").and_then(Value::as_str) {
                    parts.push(text.to_owned());
                    continue;
                }
            }
            if parts.is_empty() {
                None
            } else {
                Some(parts.join("\n"))
            }
        }
        Value::Object(object) => {
            for key in [
                "text",
                "input_text",
                "output_text",
                "summary_text",
                "content",
            ] {
                if let Some(text) = object.get(key).and_then(Value::as_str) {
                    return Some(text.to_owned());
                }
                if let Some(text) = object.get(key).and_then(codex_content_text) {
                    return Some(text);
                }
            }
            None
        }
        _ => None,
    }
}
