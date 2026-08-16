use std::sync::Arc;

use chrono::{DateTime, Utc};
use ctx_history_capture_model::normalization::provider_value_text;
use ctx_history_capture_runtime::BaseEventLookup;
use ctx_history_core::{
    derive_event_id, ActivityInvocation, ActivityJsonCapture, ActivityResult, ActivityTextCapture,
    AgentScope, CoreActivity, CoreContentPolicyStatus, CoreRecord, EventIdentityInput,
    LiteralFactKind, NativeItemKey, ProviderDeclaredFact, ProviderNativeSessionRelationship,
    SourceKey, TypedKey, CORE_ACTIVITY_REVISION, MAX_CORE_CONTENT_BYTES,
};
use serde_json::Value;
use sha2::{Digest, Sha256};

use ctx_history_jsonl::{
    fit_jsonl_activity, FallbackEventIdentityState, JsonlActivityObservedBytes,
    JsonlFamilyProjectionMode, JsonlFamilyProjector, JsonlFamilyWorkerContext, JsonlReader,
    JsonlRecordRef, JsonlSourceIdentity,
};
use ctx_history_provider_runtime::{
    source_io::ProviderSourceRoot, CaptureError, ProviderBaseEventLookup, ProviderJsonlRuntime,
    ProviderRuntimeBinding, Result,
};

use crate::mux::normalization::{
    apply_mux_core_output_diagnostic, mux_core_event, mux_event_text, mux_event_type,
    mux_message_timestamp_opt, mux_output_projection, mux_partial_event_index,
    mux_provider_event_id, mux_result_content, MuxMessageRow, MuxOutputProjection,
};

use super::{
    open_verified, MuxBinding, MuxStreamKind, EVENT_IDENTITY_REVISION, LOGICAL_EVENT_KIND,
    MAX_EVENT_SEQUENCE_ORDINAL, PARSER_REVISION, PARTIAL_EVENT_SEQUENCE_BASE,
};

const NATIVE_ITEM_NAMESPACE: &str = "mux.record";
const FALLBACK_ITEM_NAMESPACE: &str = "mux.record.fallback";
const FALLBACK_FINGERPRINT_DOMAIN: &[u8] = b"ctx.mux.fallback-event-fingerprint.v1\0";

pub(super) struct MuxProjector<L: BaseEventLookup> {
    source: SourceKey,
    authority: Arc<ProviderSourceRoot>,
    binding: MuxBinding,
    fallback_identities: FallbackEventIdentityState<L, CaptureError>,
}

impl<L> MuxProjector<L>
where
    L: BaseEventLookup,
{
    pub(super) fn new(
        source: SourceKey,
        authority: Arc<ProviderSourceRoot>,
        binding: MuxBinding,
        mode: JsonlFamilyProjectionMode,
        base_event_lookup: Option<L>,
    ) -> Result<Self> {
        let fallback_identities = FallbackEventIdentityState::new(
            source.clone(),
            binding.session_id,
            LOGICAL_EVENT_KIND,
            FALLBACK_ITEM_NAMESPACE,
            EVENT_IDENTITY_REVISION,
            mode.into(),
            base_event_lookup,
        )?;
        Ok(Self {
            source,
            authority,
            binding,
            fallback_identities,
        })
    }

    fn project_record(
        &mut self,
        stream: MuxStreamKind,
        record: JsonlRecordRef<'_>,
        emit: &mut dyn FnMut(CoreRecord) -> Result<()>,
    ) -> Result<()> {
        let bytes = record.bytes();
        if bytes.iter().all(u8::is_ascii_whitespace) {
            return Ok(());
        }
        let Ok(value) = serde_json::from_slice::<Value>(bytes) else {
            return Ok(());
        };
        if !value.is_object() {
            return Ok(());
        }
        if value
            .get("workspaceId")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .is_some_and(|owner| owner != self.binding.metadata.provider_session_id)
        {
            return Err(CaptureError::InvalidPayload(
                "Mux record changed its native session owner".to_owned(),
            ));
        }
        let output = mux_output_projection(&value);
        let content_omission = mux_output_content_omission(&value, output.as_ref());
        let evidence = record.evidence();
        let ordinal = evidence.physical_ordinal();
        if !stream.is_partial() && ordinal > MAX_EVENT_SEQUENCE_ORDINAL {
            return Err(CaptureError::InvalidPayload(
                "Mux source ordinal exceeds event identity capacity".to_owned(),
            ));
        }
        let event_sequence = if stream.is_partial() {
            PARTIAL_EVENT_SEQUENCE_BASE
                | (mux_partial_event_index(bytes) & MAX_EVENT_SEQUENCE_ORDINAL)
        } else {
            ordinal
        };
        let native_record_id = mux_provider_event_id(&value, stream.is_partial());
        let (native_item_key, native_event_id) = match native_record_id {
            Some(native_record_id) => {
                let native_event_id = TypedKey::utf8(native_record_id).map_err(contract)?;
                (
                    NativeItemKey::native_id(NATIVE_ITEM_NAMESPACE, native_event_id.clone())
                        .map_err(contract)?,
                    native_event_id,
                )
            }
            None => {
                let assignment = self
                    .fallback_identities
                    .assign(fallback_fingerprint(stream, bytes)?, None)?;
                (
                    assignment.native_item_key().clone(),
                    assignment.native_event_id().clone(),
                )
            }
        };
        let event_id = derive_event_id(EventIdentityInput {
            source: &self.source,
            session_id: self.binding.session_id,
            logical_item_kind: LOGICAL_EVENT_KIND,
            native_item_key: &native_item_key,
            subrecord_selector: None,
        })
        .map_err(contract)?;
        let row = MuxMessageRow { value };
        let occurred_at = mux_message_timestamp_opt(&row.value).unwrap_or_else(|| {
            self.binding
                .metadata
                .started_at
                .parse::<DateTime<Utc>>()
                .unwrap_or(DateTime::<Utc>::UNIX_EPOCH)
        });
        let mut event = mux_core_event(&row, occurred_at);
        if let Some(output) = output.as_ref() {
            apply_mux_core_output_diagnostic(&mut event, &row.value, output);
        }
        let body = match mux_exact_logical_content(&row.value) {
            Ok(body) => body,
            Err(_) if content_omission.is_some() => "Mux output content omitted".to_owned(),
            Err(error) => return Err(error),
        };
        if body.is_empty() {
            return Err(CaptureError::InvalidPayload(
                "Mux source-backed event has no exact lexical body".to_owned(),
            ));
        }
        let mut facts = Vec::new();
        if let Some(cwd) = &self.binding.metadata.cwd {
            facts.push(ProviderDeclaredFact {
                kind: LiteralFactKind::SessionCwd,
                value: cwd.clone(),
            });
        }
        let activity = mux_activity(&row.value, facts).map_err(contract)?;
        let mut record = CoreRecord::new_selected(
            event_id,
            self.binding.session_id,
            self.source.clone(),
            event_sequence,
            event.event_type.as_str(),
            PARSER_REVISION,
            body.clone(),
        )
        .map_err(contract)?;
        if let Some(parent_session_id) = self
            .binding
            .parent_session_id
            .filter(|_| !self.binding.metadata.lineage_ambiguous)
        {
            record.parent_session_id = Some(parent_session_id);
            record.root_session_id = Some(self.binding.root_session_id);
            record.session_relationship = Some(ProviderNativeSessionRelationship::Delegated);
            record.agent_scope = Some(AgentScope::Subagent);
        }
        record.provider_session_id = Some(self.binding.metadata.provider_session_id.clone());
        record.native_event_id = Some(native_event_id);
        record.occurred_at_unix_ms = Some(event.occurred_at.timestamp_millis());
        record.role = event.role.map(|role| role.as_str().to_owned());
        record.content.structured_content = Some(row.value);
        record.content.activity = activity;
        if let Some((kind, reason)) = content_omission {
            record.content.policy_status = CoreContentPolicyStatus::Omitted {
                reason: reason.to_owned(),
            };
            record.content.normalized_body = None;
            record.content.structured_content = None;
            let _ = kind;
        } else {
            fit_jsonl_activity(
                &body,
                record.content.structured_content.as_ref(),
                &mut record.content.activity,
                JsonlActivityObservedBytes::infer_from_present(),
                MAX_CORE_CONTENT_BYTES,
            );
            record
                .content
                .omit_structured_content_if_aggregate_exceeds_limit()
                .map_err(contract)?;
        }
        record.validate_contract().map_err(contract)?;
        emit(record)
    }
}

fn mux_activity(value: &Value, facts: Vec<ProviderDeclaredFact>) -> Result<Option<CoreActivity>> {
    let dynamic_parts = value
        .get("parts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|part| part.get("type").and_then(Value::as_str) == Some("dynamic-tool"))
        .collect::<Vec<_>>();
    let [part] = dynamic_parts.as_slice() else {
        return Ok((!facts.is_empty()).then_some(CoreActivity {
            revision: CORE_ACTIVITY_REVISION,
            provider_call_id: None,
            invocation: None,
            result: None,
            facts,
        }));
    };
    let call_ids = [
        "toolCallId",
        "tool_call_id",
        "callId",
        "call_id",
        "toolUseId",
        "tool_use_id",
        "id",
    ]
    .into_iter()
    .filter_map(|field| part.get(field).and_then(Value::as_str))
    .collect::<Vec<_>>();
    let provider_call_id = match call_ids.as_slice() {
        [id] if !id.is_empty() => Some(TypedKey::utf8(*id).map_err(contract)?),
        _ => None,
    };
    let tool = ["toolName", "tool_name", "name"]
        .into_iter()
        .filter_map(|field| part.get(field).and_then(Value::as_str))
        .collect::<Vec<_>>();
    let invocation = match tool.as_slice() {
        [tool] if !tool.is_empty() => Some(ActivityInvocation {
            protocol: None,
            server: None,
            tool: (*tool).to_owned(),
            arguments: part
                .get("input")
                .map_or(ActivityJsonCapture::Absent, |value| {
                    ActivityJsonCapture::Present {
                        value: value.clone(),
                    }
                }),
            started_at_unix_ms: None,
        }),
        _ => None,
    };
    let output_redacted = part.get("state").and_then(Value::as_str) == Some("output-redacted");
    let result = if output_redacted {
        Some(ActivityResult {
            status: None,
            completed_at_unix_ms: None,
            duration_ns: None,
            text: ActivityTextCapture::Unavailable,
            structured_content: ActivityJsonCapture::Unavailable,
        })
    } else {
        part.get("output").map(|output| ActivityResult {
            status: None,
            completed_at_unix_ms: None,
            duration_ns: None,
            text: output
                .as_str()
                .map_or(ActivityTextCapture::Absent, |value| {
                    ActivityTextCapture::Present {
                        value: value.to_owned(),
                    }
                }),
            structured_content: ActivityJsonCapture::Present {
                value: output.clone(),
            },
        })
    };
    Ok(Some(CoreActivity {
        revision: CORE_ACTIVITY_REVISION,
        provider_call_id,
        invocation,
        result,
        facts,
    }))
}

pub(super) struct MuxJsonlProjector<B: ProviderRuntimeBinding> {
    inner: MuxProjector<ProviderBaseEventLookup<B>>,
}

impl<B> MuxJsonlProjector<B>
where
    B: ProviderRuntimeBinding,
{
    pub(super) fn new(
        source: SourceKey,
        authority: Arc<ProviderSourceRoot>,
        binding: MuxBinding,
        mode: JsonlFamilyProjectionMode,
        base_event_lookup: Option<ProviderBaseEventLookup<B>>,
    ) -> Result<Self> {
        Ok(Self {
            inner: MuxProjector::new(source, authority, binding, mode, base_event_lookup)?,
        })
    }
}

fn mux_output_content_omission(
    value: &Value,
    output: Option<&MuxOutputProjection>,
) -> Option<(&'static str, &'static str)> {
    output.filter(|output| !output.body_available)?;
    let explicitly_redacted = value
        .get("parts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .any(|part| part.get("state").and_then(Value::as_str) == Some("output-redacted"));
    if explicitly_redacted {
        Some((
            "explicit_redaction",
            "Mux provider marked the tool output as redacted",
        ))
    } else {
        Some((
            "provider_private_framing",
            "Mux output framing contains no admitted textual or structured result",
        ))
    }
}

impl<B> JsonlFamilyProjector for MuxJsonlProjector<B>
where
    B: ProviderRuntimeBinding,
{
    type Runtime = ProviderJsonlRuntime<B>;

    fn project(
        &mut self,
        record: JsonlRecordRef<'_>,
        _worker: &mut JsonlFamilyWorkerContext<Self::Runtime>,
        emit: &mut dyn FnMut(CoreRecord) -> Result<()>,
    ) -> Result<()> {
        let stream = self.inner.binding.primary_stream;
        self.inner.project_record(stream, record, emit)
    }

    fn finish_projecting(
        &mut self,
        _worker: &mut JsonlFamilyWorkerContext<Self::Runtime>,
        emit: &mut dyn FnMut(CoreRecord) -> Result<()>,
    ) -> Result<()> {
        if !self.inner.binding.primary_stream.is_partial() {
            if let Some(partial) = self.inner.binding.partial.clone() {
                let source_file = open_verified(&self.inner.authority, &partial)?;
                let path = self
                    .inner
                    .authority
                    .named_path()
                    .join(&partial.relative_path);
                let mut reader = JsonlReader::open_whole_record(
                    JsonlSourceIdentity::new(
                        "mux",
                        PARSER_REVISION,
                        "mux-bounded-partial-snapshot-v1",
                        self.inner.source.exact_descriptor_digest(),
                        path,
                    ),
                    source_file,
                    None,
                )?;
                while reader
                    .visit_page(&mut |record| {
                        self.inner
                            .project_record(MuxStreamKind::Partial, record, emit)
                    })?
                    .is_some()
                {}
                if reader.outcome().is_none() {
                    return Err(CaptureError::SystemInvariant(
                        "Mux partial snapshot scan has no terminal evidence",
                    ));
                }
            }
        }
        self.inner.fallback_identities.finish()
    }
}

fn fallback_fingerprint(stream: MuxStreamKind, bytes: &[u8]) -> Result<TypedKey> {
    let mut digest = Sha256::new();
    digest.update(FALLBACK_FINGERPRINT_DOMAIN);
    digest.update([u8::from(stream.is_partial())]);
    digest.update((bytes.len() as u64).to_be_bytes());
    digest.update(bytes);
    TypedKey::bytes(digest.finalize().to_vec()).map_err(contract)
}

fn mux_exact_logical_content(value: &Value) -> Result<String> {
    let event_type = mux_event_type(value);
    if matches!(
        event_type,
        ctx_history_core::EventType::ToolOutput | ctx_history_core::EventType::CommandOutput
    ) {
        return mux_result_content(value).ok_or_else(|| {
            CaptureError::InvalidPayload("Mux exact output body is unavailable".to_owned())
        });
    }
    let mut rendered = Vec::new();
    if let Some(parts) = value.get("parts").and_then(Value::as_array) {
        for part in parts {
            match part.get("type").and_then(Value::as_str) {
                Some("text" | "reasoning") => {
                    if let Some(text) = part.get("text").and_then(Value::as_str) {
                        rendered.push(text.to_owned());
                    }
                }
                Some("dynamic-tool") => rendered.push(exact_tool_part_text(part)),
                Some("file") => {
                    if let Some(label) = exact_file_part_text(part) {
                        rendered.push(label);
                    }
                }
                _ => {
                    if let Some(text) = part.get("text").and_then(Value::as_str) {
                        rendered.push(text.to_owned());
                    }
                }
            }
        }
    }
    if !rendered.is_empty() {
        return Ok(rendered.join("\n"));
    }
    if let Some(text) = value
        .get("content")
        .or_else(|| value.get("message"))
        .and_then(provider_value_text)
    {
        return Ok(text);
    }
    Ok(mux_event_text(value, event_type))
}

fn exact_tool_part_text(part: &Value) -> String {
    let name = part
        .get("toolName")
        .or_else(|| part.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("tool");
    let state = part.get("state").and_then(Value::as_str);
    let prefix = if matches!(state, Some("output-available" | "output-redacted"))
        || part.get("output").is_some()
    {
        "tool output"
    } else {
        "tool call"
    };
    let mut text = format!("{prefix}: {name}");
    if let Some(input) = part.get("input") {
        text.push_str("\ninput: ");
        text.push_str(&exact_value_text(input));
    }
    if let Some(output) = part.get("output") {
        text.push_str("\noutput: ");
        text.push_str(&exact_value_text(output));
    }
    if let Some(nested) = part.get("nestedCalls").and_then(Value::as_array) {
        let names = nested
            .iter()
            .filter_map(|call| {
                call.get("toolName")
                    .or_else(|| call.get("name"))
                    .and_then(Value::as_str)
            })
            .collect::<Vec<_>>();
        if !names.is_empty() {
            text.push_str("\nnested tools: ");
            text.push_str(&names.join(", "));
        }
    }
    text
}

fn exact_value_text(value: &Value) -> String {
    provider_value_text(value)
        .or_else(|| serde_json::to_string(value).ok())
        .unwrap_or_else(|| value.to_string())
}

fn exact_file_part_text(part: &Value) -> Option<String> {
    let label = part
        .get("filename")
        .or_else(|| part.get("name"))
        .or_else(|| part.get("mediaType"))
        .or_else(|| part.get("mimeType"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .or_else(|| {
            part.get("url")
                .and_then(Value::as_str)
                .filter(|url| !url.starts_with("data:") && url.len() < 256)
                .map(str::to_owned)
        })?;
    Some(format!("file: {label}"))
}
fn contract(error: impl std::fmt::Display) -> CaptureError {
    CaptureError::InvalidPayload(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone)]
    struct EmptyLookup;

    impl ctx_history_capture_runtime::BaseEventLookup for EmptyLookup {
        type Error = std::convert::Infallible;

        fn contains(&self, _event_id: uuid::Uuid) -> std::result::Result<bool, Self::Error> {
            Ok(false)
        }
    }

    fn project_relationship_fixture(parent: Option<&str>) -> CoreRecord {
        let temp = tempfile::tempdir().unwrap();
        let authority = Arc::new(ProviderSourceRoot::open(temp.path()).unwrap());
        let provider_session_id = if parent.is_some() {
            "mux-child"
        } else {
            "mux-root"
        };
        let source = super::super::source_key(provider_session_id).unwrap();
        let session_id = super::super::session_identity(&source, provider_session_id).unwrap();
        let parent_session_id = parent
            .map(super::super::related_session_identity)
            .transpose()
            .unwrap();
        let binding = MuxBinding {
            metadata: crate::mux::metadata::MuxBoundedSessionMetadata {
                provider_session_id: provider_session_id.to_owned(),
                parent_provider_session_id: parent.map(str::to_owned),
                root_provider_session_id: parent.map(str::to_owned),
                lineage_ambiguous: false,
                started_at: "2026-08-05T12:00:00Z".to_owned(),
                cwd: Some("/workspace/mux".to_owned()),
                model: Some("mux-test".to_owned()),
                metadata_revision: "mux-test-metadata-v1".to_owned(),
                metadata_failure: None,
            },
            session_id,
            parent_session_id,
            root_session_id: parent_session_id.unwrap_or(session_id),
            primary_stream: MuxStreamKind::Chat,
            chat: None,
            partial: None,
            metadata_file: None,
            source_revision_digest: [7; 32],
        };
        let mut projector = MuxProjector::<EmptyLookup>::new(
            source,
            authority,
            binding,
            JsonlFamilyProjectionMode::Cold,
            None,
        )
        .unwrap();
        let value = serde_json::json!({
            "id": "mux-child-event",
            "workspaceId": provider_session_id,
            "role": "user",
            "createdAt": "2026-08-05T12:00:01Z",
            "parts": [{"type": "text", "text": "exact child-owned Mux event"}]
        });
        let bytes = serde_json::to_vec(&value).unwrap();
        let mut emitted = Vec::new();
        projector
            .project_record(
                MuxStreamKind::Chat,
                JsonlRecordRef::for_test(&bytes, 0),
                &mut |record| {
                    emitted.push(record);
                    Ok(())
                },
            )
            .unwrap();
        assert_eq!(emitted.len(), 1);
        emitted.pop().unwrap()
    }

    #[test]
    fn delegated_tasks_are_unique_while_root_events_stay_unknown() {
        let child = project_relationship_fixture(Some("mux-parent"));
        assert_eq!(
            child.session_relationship,
            Some(ProviderNativeSessionRelationship::Delegated)
        );
        assert_eq!(child.agent_scope, Some(AgentScope::Subagent));
        assert_eq!(
            child.content.meaningful_text(),
            "exact child-owned Mux event"
        );
        assert!(child.native_event_id.is_some());

        let root = project_relationship_fixture(None);
        assert_eq!(root.session_relationship, None);
        assert_eq!(root.agent_scope, None);
        assert_eq!(
            root.content.meaningful_text(),
            "exact child-owned Mux event"
        );
        assert!(root.native_event_id.is_some());
    }

    #[test]
    fn contradictory_lineage_aliases_omit_relationship_claim() {
        let temp = tempfile::tempdir().unwrap();
        let native = crate::mux::source::MuxSessionSource {
            session_dir: temp.path().join("mux-child"),
            chat_path: None,
            partial_path: None,
            metadata_path: None,
            provider_session_id: "mux-child".to_owned(),
            parent_provider_session_id: None,
        };
        let metadata = crate::mux::metadata::mux_bounded_session_metadata_from_bytes(
            &native,
            "mux-test-metadata-v2",
            "2026-08-05T12:00:00Z".parse().unwrap(),
            Some(
                &serde_json::to_vec(&serde_json::json!({
                    "workspaceId": "mux-child",
                    "parentWorkspaceId": "mux-parent",
                    "parentTaskId": "contradictory-parent",
                    "rootWorkspaceId": "mux-parent",
                    "rootTaskId": "contradictory-root"
                }))
                .unwrap(),
            ),
        )
        .unwrap();
        assert!(metadata.lineage_ambiguous);
        assert_eq!(
            metadata.parent_provider_session_id.as_deref(),
            Some("mux-parent")
        );
        assert_eq!(
            metadata.root_provider_session_id.as_deref(),
            Some("mux-parent")
        );

        let source = super::super::source_key(&metadata.provider_session_id).unwrap();
        let session_id =
            super::super::session_identity(&source, &metadata.provider_session_id).unwrap();
        let parent_session_id = super::super::related_session_identity("mux-parent").unwrap();
        let binding = MuxBinding {
            metadata,
            session_id,
            parent_session_id: Some(parent_session_id),
            root_session_id: parent_session_id,
            primary_stream: MuxStreamKind::Chat,
            chat: None,
            partial: None,
            metadata_file: None,
            source_revision_digest: [8; 32],
        };
        let authority = Arc::new(ProviderSourceRoot::open(temp.path()).unwrap());
        let mut projector = MuxProjector::<EmptyLookup>::new(
            source,
            authority,
            binding,
            JsonlFamilyProjectionMode::Cold,
            None,
        )
        .unwrap();
        let bytes = serde_json::to_vec(&serde_json::json!({
            "id": "ambiguous-lineage-event",
            "workspaceId": "mux-child",
            "role": "user",
            "createdAt": "2026-08-05T12:00:01Z",
            "parts": [{"type": "text", "text": "ambiguous Mux lineage"}]
        }))
        .unwrap();
        let mut emitted = Vec::new();
        projector
            .project_record(
                MuxStreamKind::Chat,
                JsonlRecordRef::for_test(&bytes, 0),
                &mut |record| {
                    emitted.push(record);
                    Ok(())
                },
            )
            .unwrap();

        let record = emitted.pop().unwrap();
        assert_eq!(record.session_relationship, None);
        assert_eq!(record.agent_scope, None);
    }

    #[test]
    fn provider_textual_result_over_16k_is_complete() {
        let tail = "mux_success_result_tail_complete";
        let output = format!("{} {tail}", "successful mux output ".repeat(800));
        assert!(output.len() > 16_000);
        let value = serde_json::json!({
            "role": "assistant",
            "parts": [{
                "type": "dynamic-tool",
                "toolName": "shell",
                "toolCallId": "complete-success",
                "state": "output-available",
                "output": output,
            }]
        });

        assert_eq!(mux_exact_logical_content(&value).unwrap(), output);
        assert!(
            mux_output_content_omission(&value, mux_output_projection(&value).as_ref()).is_none()
        );
    }

    #[test]
    fn explicit_redaction_has_truthful_omission_reason() {
        let value = serde_json::json!({
            "role": "assistant",
            "parts": [{
                "type": "dynamic-tool",
                "toolName": "shell",
                "toolCallId": "redacted",
                "state": "output-redacted",
            }]
        });
        assert_eq!(
            mux_output_content_omission(&value, mux_output_projection(&value).as_ref()),
            Some((
                "explicit_redaction",
                "Mux provider marked the tool output as redacted"
            ))
        );
    }
}
