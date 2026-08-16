use super::super::nativepath::{WarpNativeCounters, WarpNativeEventIdentity, WarpNativeOrder};
use super::*;
use crate::record_evidence::RecordDigest;
use ctx_history_core::{
    ActivityJsonCapture, ActivityTextCapture, CoreRecord, EventRole, EventType, TypedKey,
    CORE_ACTIVITY_REVISION, MAX_CORE_CONTENT_BYTES,
};
use rusqlite::OpenFlags;
use std::collections::HashSet;

#[derive(Default)]
struct FixtureSink {
    pages: Vec<WarpNativePage>,
}

impl WarpNativeSink for FixtureSink {
    fn push_page(&mut self, page: WarpNativePage) -> CaptureResult<()> {
        self.pages.push(page);
        Ok(())
    }
}

fn fixture_events() -> Vec<WarpNativeEvent> {
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/provider-history/warp/v1/warp-mcp.sqlite");
    let connection =
        Connection::open_with_flags(fixture, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let mut sink = FixtureSink::default();
    let scan = scan_warp_source_backed_connection(&connection, &mut sink).unwrap();
    assert_eq!(scan.counters.malformed_task_cells, 1);
    sink.pages
        .into_iter()
        .flat_map(|page| page.events)
        .collect()
}

fn fixture_source_and_lineage() -> (SourceKey, WarpSessionLineage) {
    let selection = WarpSourceSelectionV0::new("/tmp", "/tmp/warp.db", "surface").unwrap();
    (
        warp_source_key(&selection).unwrap(),
        WarpSessionLineage {
            parent_conversation_id: None,
        },
    )
}

fn activity_for_call<'a>(
    records: &'a [CoreRecord],
    call_id: &str,
    event_type: EventType,
    server: &str,
    tool: &str,
) -> &'a CoreActivity {
    let key = TypedKey::Utf8(call_id.to_owned());
    records
        .iter()
        .find(|record| {
            record.event_type == event_type.as_str()
                && record.content.activity.as_ref().is_some_and(|activity| {
                    activity.provider_call_id.as_ref() == Some(&key)
                        && activity.invocation.as_ref().is_some_and(|invocation| {
                            invocation.server.as_deref() == Some(server) && invocation.tool == tool
                        })
                })
        })
        .and_then(|record| record.content.activity.as_ref())
        .unwrap()
}

fn record_for_call<'a>(
    records: &'a [CoreRecord],
    call_id: &str,
    event_type: EventType,
    server: &str,
    tool: &str,
) -> &'a CoreRecord {
    let key = TypedKey::Utf8(call_id.to_owned());
    records
        .iter()
        .find(|record| {
            record.event_type == event_type.as_str()
                && record.content.activity.as_ref().is_some_and(|activity| {
                    activity.provider_call_id.as_ref() == Some(&key)
                        && activity.invocation.as_ref().is_some_and(|invocation| {
                            invocation.server.as_deref() == Some(server) && invocation.tool == tool
                        })
                })
        })
        .unwrap()
}

#[test]
fn scan_accounting_includes_unknown_message_units() {
    let native_scan = WarpNativeSourceBackedScan {
        source_integrity_digest: "00".repeat(32),
        counters: WarpNativeCounters {
            sessions_retained: 1,
            ignored_messages: 1,
            ..WarpNativeCounters::default()
        },
    };
    assert_eq!(accounted_ignored_records(&native_scan, 1).unwrap(), 2);
}

#[test]
fn core_projection_keeps_success_failure_unknown_and_large_result_bodies_once() {
    let (source, lineage) = fixture_source_and_lineage();
    for (index, body) in [
        format!(
            "warp-core-head-{}-warp-core-tail",
            "x".repeat(8 * 1024 * 1024)
        ),
        "failure complete Warp result".to_owned(),
        "unknown complete Warp result".to_owned(),
    ]
    .into_iter()
    .enumerate()
    {
        let event = WarpNativeEvent {
            identity: WarpNativeEventIdentity {
                conversation_id: "conversation".to_owned(),
                task_id: format!("task-{index}"),
                message: WarpNativeMessageIdentity::MessageOrdinal(0),
            },
            native_order: WarpNativeOrder {
                provider_event_index: u64::try_from(index).unwrap(),
                legacy_provider_event_index: Some(u64::try_from(index).unwrap()),
                task_rowid: i64::try_from(index + 1).unwrap(),
                task_key: format!("task-{index}"),
                message_ordinal: 0,
            },
            event_type: EventType::ToolOutput,
            role: Some(EventRole::Tool),
            kind: "run_shell_command",
            request_id: Some(format!("request-{index}")),
            call_id: Some(format!("call-{index}")),
            mcp_invocation: None,
            mcp_response: None,
            mcp_attribution: false,
            occurred_at: None,
            lexical_body: body.clone(),
            source_record_digest: RecordDigest::from_text("warp source row"),
        };
        let record = core_record(&source, &lineage, event).unwrap();
        assert_eq!(record.content.meaningful_text(), body);
        assert_eq!(
            record
                .content
                .structured_content
                .as_ref()
                .and_then(|value| value.get("call_id"))
                .and_then(serde_json::Value::as_str),
            Some(format!("call-{index}").as_str())
        );
        assert!(!record
            .content
            .structured_content
            .as_ref()
            .unwrap()
            .to_string()
            .contains("complete Warp result"));
        record.validate_contract().unwrap();
    }
}

#[test]
fn sanitized_mcp_fixture_projects_only_unique_qualified_terminal_pairs() {
    let events = fixture_events();
    let (source, lineage) = fixture_source_and_lineage();

    // Ambiguous/orphan pairs remain as native records but never acquire activity.
    for event in events.iter().filter(|event| !event.mcp_attribution) {
        let record = core_record(&source, &lineage, event.clone()).unwrap();
        if event.event_type == EventType::ToolOutput {
            assert!(record.content.activity.is_none());
        }
    }

    let records = events
        .iter()
        .cloned()
        .map(|event| core_record(&source, &lineage, event).unwrap())
        .collect::<Vec<_>>();

    let call_links = records
        .iter()
        .filter(|record| record.event_type == EventType::ToolCall.as_str())
        .filter_map(|record| {
            let activity = record.content.activity.as_ref()?;
            let invocation = activity.invocation.as_ref()?;
            Some((
                activity.provider_call_id.clone(),
                invocation.server.clone(),
                invocation.tool.clone(),
            ))
        })
        .collect::<HashSet<_>>();
    let result_links = records
        .iter()
        .filter(|record| record.event_type == EventType::ToolOutput.as_str())
        .filter_map(|record| {
            let activity = record.content.activity.as_ref()?;
            activity.result.as_ref()?;
            let invocation = activity.invocation.as_ref()?;
            Some((
                activity.provider_call_id.clone(),
                invocation.server.clone(),
                invocation.tool.clone(),
            ))
        })
        .collect::<HashSet<_>>();
    assert_eq!(call_links, result_links);

    let success_call = activity_for_call(
        &records,
        "success",
        EventType::ToolCall,
        "11111111-1111-4111-8111-111111111111",
        "shared_tool",
    );
    let success_invocation = success_call.invocation.as_ref().unwrap();
    assert_eq!(
        success_invocation.server.as_deref(),
        Some("11111111-1111-4111-8111-111111111111")
    );
    assert_eq!(success_invocation.tool, "shared_tool");
    assert_eq!(
        success_invocation.arguments,
        ActivityJsonCapture::Present {
            value: serde_json::json!({"side": "a"}),
        }
    );

    let success_result = activity_for_call(
        &records,
        "success",
        EventType::ToolOutput,
        "11111111-1111-4111-8111-111111111111",
        "shared_tool",
    );
    let linked_invocation = success_result.invocation.as_ref().unwrap();
    assert_eq!(
        linked_invocation.server.as_deref(),
        Some("11111111-1111-4111-8111-111111111111")
    );
    assert_eq!(linked_invocation.tool, "shared_tool");
    let result = success_result.result.as_ref().unwrap();
    assert_eq!(result.status, None);
    assert_eq!(result.text, ActivityTextCapture::NormalizedBody);

    let final_result = activity_for_call(
        &records,
        "final-id",
        EventType::ToolOutput,
        "11111111-1111-4111-8111-111111111111",
        "final_tool",
    );
    assert_eq!(
        final_result.invocation.as_ref().unwrap().arguments,
        ActivityJsonCapture::Present {
            value: serde_json::json!({"first": "one", "second": true}),
        }
    );
    assert_eq!(
        final_result.result.as_ref().unwrap().text,
        ActivityTextCapture::NormalizedBody
    );

    for (call_id, server, tool, body, text, structured) in [
        (
            "success",
            "11111111-1111-4111-8111-111111111111",
            "shared_tool",
            "first\nresource text\nlast",
            ActivityTextCapture::NormalizedBody,
            "unavailable",
        ),
        (
            "error",
            "22222222-2222-4222-8222-222222222222",
            "shared_tool",
            "sanitized tool error",
            ActivityTextCapture::NormalizedBody,
            "present",
        ),
        (
            "cancel",
            "11111111-1111-4111-8111-111111111111",
            "cancel_tool",
            "cancel",
            ActivityTextCapture::Absent,
            "absent",
        ),
        (
            "nontext",
            "22222222-2222-4222-8222-222222222222",
            "binary_tool",
            "call_mcp_tool",
            ActivityTextCapture::Absent,
            "unavailable",
        ),
        (
            "success",
            "22222222-2222-4222-8222-222222222222",
            "reused_tool",
            "call_mcp_tool",
            ActivityTextCapture::Absent,
            "present",
        ),
    ] {
        let record = record_for_call(&records, call_id, EventType::ToolOutput, server, tool);
        assert_eq!(record.content.meaningful_text(), body);
        let result = record
            .content
            .activity
            .as_ref()
            .and_then(|activity| activity.result.as_ref())
            .unwrap();
        assert_eq!(result.status, None);
        assert_eq!(result.text, text);
        assert_eq!(
            match result.structured_content {
                ActivityJsonCapture::Absent => "absent",
                ActivityJsonCapture::Present { .. } => "present",
                ActivityJsonCapture::Unavailable => "unavailable",
                ActivityJsonCapture::Omitted { .. } => "omitted",
            },
            structured,
            "unexpected structured capture for {call_id} on {server}/{tool}"
        );
        record.validate_contract().unwrap();
    }
    assert!(records.iter().all(|record| {
        record
            .content
            .structured_content
            .as_ref()
            .is_none_or(|value| !value.to_string().contains("c2FuaXRpemVk"))
    }));

    let mut oversized_invocation_event = events
        .iter()
        .find(|event| {
            event.event_type == EventType::ToolCall
                && event.call_id.as_deref() == Some("success")
                && event.mcp_invocation.as_ref().is_some_and(|invocation| {
                    invocation.server_id == "11111111-1111-4111-8111-111111111111"
                        && invocation.tool_name == "shared_tool"
                })
        })
        .unwrap()
        .clone();
    oversized_invocation_event
        .mcp_invocation
        .as_mut()
        .unwrap()
        .args = serde_json::json!({"oversized": "x".repeat(MAX_CORE_CONTENT_BYTES)});
    let oversized_invocation = core_record(&source, &lineage, oversized_invocation_event).unwrap();
    let oversized_activity = oversized_invocation.content.activity.as_ref().unwrap();
    assert_eq!(
        oversized_activity.provider_call_id,
        Some(TypedKey::Utf8("success".to_owned()))
    );
    let ActivityJsonCapture::Omitted {
        reason,
        observed_encoded_bytes,
    } = &oversized_activity.invocation.as_ref().unwrap().arguments
    else {
        panic!("oversized Warp invocation arguments were not explicitly omitted");
    };
    assert_eq!(reason, "size_limit");
    assert!(observed_encoded_bytes
        .is_some_and(|bytes| { bytes > u64::try_from(MAX_CORE_CONTENT_BYTES).unwrap() }));
    oversized_invocation.validate_contract().unwrap();

    // Exercise the aggregate boundary: complete 8 MiB body plus a complete
    // 8 MiB result payload must preserve linkage/metadata and explicitly omit
    // only the oversized structured result channel.
    let mut large_event = events
        .iter()
        .find(|event| {
            event.event_type == EventType::ToolOutput
                && event.call_id.as_deref() == Some("success")
                && event.mcp_attribution
                && event.mcp_invocation.as_ref().is_some_and(|invocation| {
                    invocation.server_id == "11111111-1111-4111-8111-111111111111"
                        && invocation.tool_name == "shared_tool"
                })
        })
        .unwrap()
        .clone();
    let large_body = format!(
        "warp-large-head-{}-warp-large-tail",
        "b".repeat(8 * 1024 * 1024)
    );
    large_event.lexical_body = large_body.clone();
    let response = large_event.mcp_response.as_mut().unwrap();
    response.status = Some("provider-literal-status".to_owned());
    response.structured_content = ActivityJsonCapture::Present {
        value: serde_json::json!({"large": "x".repeat(8 * 1024 * 1024)}),
    };
    let large_record = core_record(&source, &lineage, large_event).unwrap();
    assert_eq!(large_record.content.meaningful_text(), large_body);
    let large_activity = large_record.content.activity.as_ref().unwrap();
    assert_eq!(
        large_activity.provider_call_id,
        Some(TypedKey::Utf8("success".to_owned()))
    );
    let invocation = large_activity.invocation.as_ref().unwrap();
    assert_eq!(
        invocation.server.as_deref(),
        Some("11111111-1111-4111-8111-111111111111")
    );
    assert_eq!(invocation.tool, "shared_tool");
    let result = large_activity.result.as_ref().unwrap();
    assert_eq!(result.status.as_deref(), Some("provider-literal-status"));
    assert_eq!(result.text, ActivityTextCapture::NormalizedBody);
    let ActivityJsonCapture::Omitted {
        reason,
        observed_encoded_bytes,
    } = &result.structured_content
    else {
        panic!("large Warp result channel was not explicitly omitted");
    };
    assert_eq!(reason, "size_limit");
    assert!(observed_encoded_bytes.is_some_and(|bytes| bytes > 8 * 1024 * 1024));
    large_record.validate_contract().unwrap();
}

#[test]
fn warp_activity_uses_neutral_core_revision_and_explicit_capture_dispositions() {
    let events = fixture_events();
    let (source, lineage) = fixture_source_and_lineage();
    let event = events
        .into_iter()
        .find(|event| event.call_id.as_deref() == Some("success") && event.mcp_attribution)
        .unwrap();
    let record = core_record(&source, &lineage, event).unwrap();
    let activity = record.content.activity.as_ref().unwrap();
    assert_eq!(activity.revision, CORE_ACTIVITY_REVISION);
    assert!(matches!(
        activity
            .result
            .as_ref()
            .map(|result| &result.structured_content),
        Some(ActivityJsonCapture::Present { .. }) | Some(ActivityJsonCapture::Unavailable) | None
    ));
}
