use crate::analytics::{BytesBucket, CountBucket};

use super::*;

fn foreground(event: &PublicEventV1) -> &ForegroundProviderRefreshV1 {
    let PublicEventV1::ProviderRefreshCompleted(event) = event else {
        panic!("expected a provider refresh event");
    };
    event.foreground.as_ref().unwrap()
}

fn record_success(
    collector: &mut ProviderRefreshCollector,
    provider: CaptureProvider,
    trigger: ProviderRefreshTrigger,
    summary: &ProviderImportSummary,
    stats: &SourceStats,
) {
    collector.record_success_with_facts(
        provider,
        trigger,
        summary,
        stats,
        ProviderRefreshRuntimeFacts::observed_success(Duration::ZERO, summary),
    );
}

#[test]
fn aggregates_coarse_work_once_per_provider() {
    let mut collector = ProviderRefreshCollector::default();
    let first = ProviderImportSummary {
        imported_sessions: 1,
        imported_events: 2,
        ..ProviderImportSummary::default()
    };
    let second = ProviderImportSummary {
        imported_sessions: 2,
        imported_events: 5,
        work_remaining: true,
        ..ProviderImportSummary::default()
    };
    record_success(
        &mut collector,
        CaptureProvider::Codex,
        ProviderRefreshTrigger::Setup,
        &first,
        &SourceStats {
            bytes: 1024,
            ..SourceStats::default()
        },
    );
    record_success(
        &mut collector,
        CaptureProvider::Codex,
        ProviderRefreshTrigger::Setup,
        &second,
        &SourceStats {
            bytes: 2048,
            ..SourceStats::default()
        },
    );

    let events = collector.finish();

    assert_eq!(events.len(), 1);
    let refresh = foreground(&events[0]);
    assert_eq!(refresh.provider, Some(CaptureProvider::Codex));
    assert_eq!(refresh.change, ProviderRefreshChange::Changed);
    assert_eq!(refresh.refresh_result, ProviderRefreshResult::Complete);
    assert_eq!(refresh.core_result, ProviderCoreResult::Complete);
    assert!(refresh.work_remaining);
    let counts = refresh.counts.as_ref().expect("coarse refresh counts");
    assert_eq!(counts.records, Some(CountBucket::SixToTwenty));
    assert_eq!(counts.logical_bytes, Some(BytesBucket::UnderOneHundredKb));
}

#[test]
fn distinguishes_no_op_from_changed() {
    let mut collector = ProviderRefreshCollector::default();
    record_success(
        &mut collector,
        CaptureProvider::Codex,
        ProviderRefreshTrigger::Search,
        &ProviderImportSummary::default(),
        &SourceStats::default(),
    );

    let events = collector.finish();
    let refresh = foreground(&events[0]);

    assert_eq!(refresh.change, ProviderRefreshChange::NoOp);
    assert_eq!(refresh.refresh_result, ProviderRefreshResult::Complete);
    assert_eq!(refresh.core_result, ProviderCoreResult::NoOp);
}

#[test]
fn exact_provider_durations_are_independent_in_multi_provider_batches() {
    let mut collector = ProviderRefreshCollector::default();
    let summary = ProviderImportSummary {
        imported_events: 1,
        ..ProviderImportSummary::default()
    };
    for (provider, duration) in [
        (CaptureProvider::Codex, Duration::from_millis(40)),
        (CaptureProvider::Claude, Duration::from_secs(7)),
    ] {
        collector.record_success_with_facts(
            provider,
            ProviderRefreshTrigger::Import,
            &summary,
            &SourceStats::default(),
            ProviderRefreshRuntimeFacts::success(duration, ProviderCoreResult::Complete),
        );
    }

    collector.refresh_duration = Duration::from_secs(90);
    let events = collector.finish();
    let duration_for = |provider| {
        events
            .iter()
            .find_map(|event| {
                (foreground(event).provider == Some(provider)).then(|| {
                    let PublicEventV1::ProviderRefreshCompleted(event) = event else {
                        unreachable!();
                    };
                    event.duration
                })
            })
            .unwrap()
    };

    assert_eq!(
        duration_for(CaptureProvider::Codex),
        DurationBucket::UnderOneHundredMs
    );
    assert_eq!(
        duration_for(CaptureProvider::Claude),
        DurationBucket::UnderThirtySeconds
    );
}

#[test]
fn every_capture_provider_emits_without_usage_suppression() {
    let providers = CaptureProvider::variants()
        .iter()
        .map(|provider| provider.parse::<CaptureProvider>().unwrap())
        .collect::<Vec<_>>();
    let mut collector = ProviderRefreshCollector::default();
    for provider in providers.iter().copied() {
        record_success(
            &mut collector,
            provider,
            ProviderRefreshTrigger::Search,
            &ProviderImportSummary::default(),
            &SourceStats::default(),
        );
    }

    let events = collector.finish();

    assert_eq!(events.len(), providers.len());
    for provider in providers {
        assert!(events
            .iter()
            .any(|event| foreground(event).provider == Some(provider)));
    }
}

#[test]
fn core_publication_emits_only_global_authoritative_facts() {
    let mut collector = ProviderRefreshCollector::default();
    collector.record_core_publication(ProviderRefreshTrigger::Import, false, 0, 0);

    let events = collector.finish();
    let refresh = foreground(&events[0]);

    assert_eq!(refresh.provider, None);
    assert_eq!(refresh.trigger, ProviderRefreshTrigger::Import);
    assert_eq!(refresh.change, ProviderRefreshChange::NoOp);
    assert_eq!(refresh.refresh_result, ProviderRefreshResult::Complete);
    assert_eq!(refresh.core_result, ProviderCoreResult::NoOp);
    assert_eq!(refresh.failure_scope, ProviderRefreshFailureScope::None);
    assert_eq!(refresh.failure_type, ProviderRefreshFailureType::None);
    assert_eq!(refresh.counts, None);
}

#[test]
fn core_publication_keeps_bounded_record_failure() {
    let mut collector = ProviderRefreshCollector::default();
    collector.record_core_publication(ProviderRefreshTrigger::Import, true, 0, 3);

    let events = collector.finish();
    let refresh = foreground(&events[0]);

    assert_eq!(refresh.refresh_result, ProviderRefreshResult::Complete);
    assert_eq!(refresh.failure_scope, ProviderRefreshFailureScope::Record);
    assert_eq!(
        refresh.failure_type,
        ProviderRefreshFailureType::RecordRejection
    );
}
