use std::{cell::Cell, time::Duration};

use uuid::Uuid;

use super::private_tempdir;
use crate::{
    local_usage::{
        ContextCoverage, LocalUsageStorageAuthority, McpCompletionFacts, McpContextTarget,
        McpCorrelationFact, McpInvocation, McpUsageRecorder, ProOutcome, SearchContextObservation,
        UsageControlSnapshot, ValueClass,
    },
    operation_descriptor::ObservedMcpProductOperation,
};

fn invocation(operation: ObservedMcpProductOperation) -> McpInvocation {
    McpInvocation::from_operation(operation)
}

#[test]
fn mcp_search_records_transport_bytes_and_adapter_supplied_canonical_context() {
    let mut invocation = invocation(ObservedMcpProductOperation::Search);
    invocation.bind_search_context(SearchContextObservation::complete(12, 40).unwrap());
    let completed = invocation.completed(
        &McpCompletionFacts {
            result_count: Some(1),
            delivered_output_bytes: 777,
            ..McpCompletionFacts::default()
        },
        Duration::from_millis(5),
    );
    assert_eq!(
        completed.result_metadata_for_test(),
        (ValueClass::ResultBearing, 1, 0)
    );
    assert_eq!(
        completed.context_metadata_for_test(),
        (ContextCoverage::Complete, 12, 40)
    );
    assert_eq!(completed.delivered_output_bytes, 777);
}

#[test]
fn mcp_blame_uses_only_bounded_completion_facts() {
    let completed = invocation(ObservedMcpProductOperation::Blame).completed(
        &McpCompletionFacts {
            result_count: Some(0),
            citation_count: 2,
            pro_outcome: Some(ProOutcome::Possible),
            delivered_output_bytes: 200,
            ..McpCompletionFacts::default()
        },
        Duration::ZERO,
    );
    assert_eq!(completed.citation_count, 2);
}

#[test]
fn bounded_uuid_correlation_never_stores_raw_selectors() {
    let root = private_tempdir();
    let authority = LocalUsageStorageAuthority::new(root.path().join("usage.sqlite"), "1.0.0");
    let mut recorder =
        McpUsageRecorder::start(authority, || UsageControlSnapshot::unversioned(true));
    let target =
        McpContextTarget::Session(Uuid::parse_str("11111111-2222-3333-4444-555555555555").unwrap());
    let found = McpCompletionFacts {
        correlation: vec![McpCorrelationFact::Found(target)],
        ..McpCompletionFacts::default()
    };
    assert!(!recorder.correlate_delivered_for_test(&found));
    let opened = McpCompletionFacts {
        correlation: vec![McpCorrelationFact::Opened(target)],
        ..McpCompletionFacts::default()
    };
    assert!(recorder.correlate_delivered_for_test(&opened));
    assert!(!recorder.correlate_delivered_for_test(&opened));
}

#[test]
fn recorder_counts_one_delivered_mcp_response_once() {
    let root = private_tempdir();
    let authority = LocalUsageStorageAuthority::new(root.path().join("usage.sqlite"), "1.0.0");
    let mut recorder =
        McpUsageRecorder::start(authority, || UsageControlSnapshot::unversioned(true));
    recorder.record_delivered(
        invocation(ObservedMcpProductOperation::Sources),
        Duration::ZERO,
        || McpCompletionFacts {
            result_count: Some(0),
            delivered_output_bytes: 123,
            ..McpCompletionFacts::default()
        },
    );
    let report = crate::local_usage::read_report(root.path(), true, true);
    let current = report.definitions.unwrap().remove(0);
    assert_eq!(current.ctx_versions, ["1.0.0"]);
    assert_eq!(current.summary.calls, 1);
    assert_eq!(current.summary.delivered_output_bytes, 123);
}

#[test]
fn disabled_recorder_never_invokes_the_completion_adapter_or_opens_sqlite() {
    let root = private_tempdir();
    let database = root.path().join("usage.sqlite");
    let authority = LocalUsageStorageAuthority::new(database.clone(), "1.0.0");
    let mut recorder =
        McpUsageRecorder::start(authority, || UsageControlSnapshot::unversioned(false));
    let completion_adapter_called = Cell::new(false);

    recorder.record_delivered(
        invocation(ObservedMcpProductOperation::Search),
        Duration::ZERO,
        || {
            completion_adapter_called.set(true);
            McpCompletionFacts::default()
        },
    );

    assert!(!completion_adapter_called.get());
    assert!(!database.exists());
}

#[test]
fn query_events_projects_to_show_event_without_raw_protocol_input() {
    let completed = invocation(ObservedMcpProductOperation::QueryEvents).completed(
        &McpCompletionFacts {
            result_count: Some(2),
            delivered_output_bytes: 321,
            ..McpCompletionFacts::default()
        },
        Duration::ZERO,
    );
    assert_eq!(
        completed.operation,
        crate::operation_descriptor::LocalUsageOperation::ShowEvent
    );
    assert_eq!(completed.result_count, 2);
}
