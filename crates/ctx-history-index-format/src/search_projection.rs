use ctx_history_core::{
    ActivityJsonCapture, ActivityTextCapture, CoreContent, CoreContentPolicyStatus,
};

use crate::{IndexError, Result};

/// Derives the complete analyzed text stored in the lexical `body_search` field.
///
/// The projection is repository-neutral and follows the exact retained Core
/// content order: normalized body, structured content, invocation, result, and
/// provider-declared literal facts. Capture dispositions and provider call IDs
/// are not lexical content. A `NormalizedBody` result reference is not repeated.
pub fn project_body_search(mut content: CoreContent) -> Result<Option<String>> {
    if !content.is_discovery_eligible()
        || !matches!(content.policy_status, CoreContentPolicyStatus::Selected)
    {
        return Ok(None);
    }

    let mut projection = content
        .normalized_body
        .take()
        .filter(|body| !body.is_empty());
    if let Some(structured_content) = content.structured_content.take() {
        append_json(&mut projection, structured_content)?;
    }

    if let Some(activity) = content.activity.take() {
        if let Some(invocation) = activity.invocation {
            append_optional_text(&mut projection, invocation.protocol)?;
            append_optional_text(&mut projection, invocation.server)?;
            append_text(&mut projection, invocation.tool)?;
            append_json_capture(&mut projection, invocation.arguments)?;
        }
        if let Some(result) = activity.result {
            append_optional_text(&mut projection, result.status)?;
            if let ActivityTextCapture::Present { value } = result.text {
                append_text(&mut projection, value)?;
            }
            append_json_capture(&mut projection, result.structured_content)?;
        }
        for fact in activity.facts {
            append_text(&mut projection, fact.value)?;
        }
    }

    Ok(projection)
}

fn append_json_capture(
    projection: &mut Option<String>,
    capture: ActivityJsonCapture,
) -> Result<()> {
    if let ActivityJsonCapture::Present { value } = capture {
        append_json(projection, value)?;
    }
    Ok(())
}

fn append_json(projection: &mut Option<String>, value: serde_json::Value) -> Result<()> {
    append_text(projection, serde_json::to_string(&value)?)
}

fn append_optional_text(projection: &mut Option<String>, value: Option<String>) -> Result<()> {
    if let Some(value) = value {
        append_text(projection, value)?;
    }
    Ok(())
}

fn append_text(projection: &mut Option<String>, value: String) -> Result<()> {
    if value.is_empty() {
        return Ok(());
    }
    match projection {
        Some(projection) => {
            projection
                .len()
                .checked_add(value.len())
                .and_then(|bytes| bytes.checked_add(1))
                .ok_or(IndexError::CountOverflow)?;
            projection.reserve(value.len() + 1);
            projection.push('\n');
            projection.push_str(&value);
        }
        None => *projection = Some(value),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use ctx_history_core::{
        ActivityInvocation, ActivityResult, CoreActivity, LiteralFactKind, ProviderDeclaredFact,
        CORE_ACTIVITY_REVISION, CORE_CONTENT_POLICY_REVISION,
    };

    use super::*;

    fn selected_content(normalized_body: Option<&str>) -> CoreContent {
        CoreContent {
            policy_revision: CORE_CONTENT_POLICY_REVISION,
            policy_status: CoreContentPolicyStatus::Selected,
            normalized_body: normalized_body.map(str::to_owned),
            structured_content: None,
            discovery_exclusion: None,
            activity: None,
        }
    }

    fn activity() -> CoreActivity {
        CoreActivity {
            revision: CORE_ACTIVITY_REVISION,
            provider_call_id: Some(ctx_history_core::TypedKey::U64(7)),
            invocation: Some(ActivityInvocation {
                protocol: Some("mcp".to_owned()),
                server: Some("服务器".to_owned()),
                tool: "lookup_tool".to_owned(),
                arguments: ActivityJsonCapture::Present {
                    value: serde_json::json!({"argument_key": "argument value"}),
                },
                started_at_unix_ms: Some(10),
            }),
            result: Some(ActivityResult {
                status: Some("provider::ok".to_owned()),
                completed_at_unix_ms: Some(20),
                duration_ns: Some(30),
                text: ActivityTextCapture::NormalizedBody,
                structured_content: ActivityJsonCapture::Present {
                    value: serde_json::json!({"result_key": "result value"}),
                },
            }),
            facts: vec![
                ProviderDeclaredFact {
                    kind: LiteralFactKind::Branch,
                    value: "Feature/ExactCase".to_owned(),
                },
                ProviderDeclaredFact {
                    kind: LiteralFactKind::File,
                    value: "file:///Work/Repo/src/lib.rs".to_owned(),
                },
            ],
        }
    }

    #[test]
    fn ordinary_body_projection_moves_and_reuses_the_body_allocation() {
        let content = selected_content(Some("ordinary normalized body"));
        let body_pointer = content.normalized_body.as_ref().unwrap().as_ptr();
        let projection = project_body_search(content).unwrap().unwrap();

        assert_eq!(projection.as_ptr(), body_pointer);
        assert_eq!(projection, "ordinary normalized body");
    }

    #[test]
    fn retrieval_derived_content_has_no_body_projection() {
        let mut content = selected_content(Some("retrieval payload canary"));
        content.discovery_exclusion =
            Some(ctx_history_core::CoreDiscoveryExclusion::CtxRetrievalDerived);

        assert_eq!(project_body_search(content).unwrap(), None);
    }

    #[test]
    fn activity_and_literal_facts_extend_complete_content_in_exact_order() {
        let mut body = String::with_capacity(512);
        body.push_str("normalized body");
        let body_pointer = body.as_ptr();
        let mut content = selected_content(None);
        content.normalized_body = Some(body);
        content.structured_content = Some(serde_json::json!({
            "top_level_key": "top level value"
        }));
        content.activity = Some(activity());

        let projection = project_body_search(content).unwrap().unwrap();

        assert_eq!(projection.as_ptr(), body_pointer);
        assert_eq!(
            projection,
            "normalized body\n{\"top_level_key\":\"top level value\"}\nmcp\n服务器\nlookup_tool\n{\"argument_key\":\"argument value\"}\nprovider::ok\n{\"result_key\":\"result value\"}\nFeature/ExactCase\nfile:///Work/Repo/src/lib.rs"
        );
        assert!(!projection.contains("NormalizedBody"));
        assert!(!projection.ends_with('\n'));
    }

    #[test]
    fn present_result_text_is_indexed_but_capture_dispositions_are_not() {
        let mut content = selected_content(None);
        let mut activity = activity();
        let result = activity.result.as_mut().unwrap();
        result.text = ActivityTextCapture::Present {
            value: "complete terminal text".to_owned(),
        };
        result.structured_content = ActivityJsonCapture::Omitted {
            reason: "size limit canary".to_owned(),
            observed_encoded_bytes: Some(998_877),
        };
        content.activity = Some(activity);

        let projection = project_body_search(content).unwrap().unwrap();
        assert!(projection.contains("complete terminal text"));
        assert!(!projection.contains("size limit canary"));
        assert!(!projection.contains("998877"));
    }
}
