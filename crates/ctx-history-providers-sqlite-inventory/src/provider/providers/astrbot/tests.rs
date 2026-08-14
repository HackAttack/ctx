use serde_json::json;

use super::model::{checkpoint_id, item_is_output, item_role, item_text};
use ctx_history_core::EventRole;

#[test]
fn astrbot_native_item_semantics_preserve_checkpoint_roles_and_complete_output_text() {
    let checkpoint = json!({"type": "checkpoint", "id": "checkpoint-7"});
    assert_eq!(checkpoint_id(&checkpoint).as_deref(), Some("checkpoint-7"));

    let output = json!({
        "role": "tool",
        "content": "complete output"
    });
    assert_eq!(item_role(&output), Some(EventRole::Tool));
    assert!(item_is_output(&output));
    assert_eq!(item_text(&output).as_deref(), Some("complete output"));
}
