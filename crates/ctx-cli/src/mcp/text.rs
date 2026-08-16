use serde_json::Value;

pub(crate) fn render_tool_text(value: &Value) -> String {
    ctx_cli_presentation::mcp_text::render_tool_text(value)
}
