use ctx_history_core::{
    ProjectionContractError, SourceAnchor, SourceInventoryObservation, TypedKey,
};

use super::model::AttemptHistoryProgressSnapshot;

use super::*;

fn descriptor(schema_variant: &str, lineage: u8) -> SourceKey {
    SourceKey::derive(
        CaptureProvider::Gemini.as_str(),
        "ownership-test",
        schema_variant,
        1,
        SourceAnchor::CatalogLineage([lineage; 32]),
    )
    .unwrap()
}

fn inventory_owner(
    route_index: usize,
    authority: u8,
    sources: Vec<SourceKey>,
) -> CompleteInventoryOwner {
    let observation = SourceInventoryObservation::new(
        CaptureProvider::Gemini.as_str(),
        "ownership-test-root",
        TypedKey::U64(u64::from(authority)),
        "ownership-test-revision",
        vec![authority],
    )
    .unwrap();
    CompleteInventoryOwner {
        route_index,
        inventory: CertifiedSourceInventory::certify(
            observation.clone(),
            observation,
            "ownership-test-discovery",
            sources,
        )
        .unwrap(),
    }
}

#[test]
fn base_ownership_accepts_exact_or_one_inventory_certified_descriptor_replacement() {
    let descriptor_a = descriptor("schema-a", 1);
    let descriptor_b = descriptor("schema-b", 1);
    let exact_owner = SourceOwner {
        route_index: 3,
        source: descriptor_a.clone(),
        present: true,
        revalidation: None,
    };
    assert!(source_owner_covers_base_source(
        &descriptor_a,
        &exact_owner,
        &[]
    ));

    let replacement_owner = SourceOwner {
        route_index: 3,
        source: descriptor_b.clone(),
        present: true,
        revalidation: None,
    };
    let inventory = inventory_owner(3, 1, vec![descriptor_b]);
    assert!(source_owner_covers_base_source(
        &descriptor_a,
        &replacement_owner,
        &[inventory]
    ));
}

#[test]
fn descriptor_replacement_ownership_rejects_absence_wrong_route_ambiguity_and_lineage() {
    let descriptor_a = descriptor("schema-a", 1);
    let descriptor_b = descriptor("schema-b", 1);
    let replacement_owner = SourceOwner {
        route_index: 3,
        source: descriptor_b.clone(),
        present: true,
        revalidation: None,
    };

    assert!(!source_owner_covers_base_source(
        &descriptor_a,
        &replacement_owner,
        &[]
    ));
    assert!(!source_owner_covers_base_source(
        &descriptor_a,
        &replacement_owner,
        &[inventory_owner(4, 1, vec![descriptor_b.clone()])]
    ));
    assert!(!source_owner_covers_base_source(
        &descriptor_a,
        &replacement_owner,
        &[
            inventory_owner(3, 1, vec![descriptor_b.clone()]),
            inventory_owner(3, 2, vec![descriptor_b]),
        ]
    ));

    let unrelated_owner = SourceOwner {
        route_index: 3,
        source: descriptor("schema-b", 2),
        present: true,
        revalidation: None,
    };
    assert!(!source_owner_covers_base_source(
        &descriptor_a,
        &unrelated_owner,
        &[inventory_owner(3, 3, vec![unrelated_owner.source.clone()])]
    ));
}

#[test]
fn inventory_rejects_two_descriptors_for_one_canonical_lineage() {
    let descriptor_a = descriptor("schema-a", 1);
    let descriptor_b = descriptor("schema-b", 1);
    let observation = SourceInventoryObservation::new(
        CaptureProvider::Gemini.as_str(),
        "ownership-test-root",
        TypedKey::U64(1),
        "ownership-test-revision",
        vec![1],
    )
    .unwrap();
    assert_eq!(
        CertifiedSourceInventory::certify(
            observation.clone(),
            observation,
            "ownership-test-discovery",
            vec![descriptor_a, descriptor_b],
        )
        .unwrap_err(),
        ProjectionContractError::DuplicateInventorySource
    );
}

#[test]
fn source_record_progress_is_prompt_throttled_monotonic_and_flushable() {
    let started = Instant::now();
    let mut progress = SourceRecordProgress::default();
    let accepted = SourceBackedRecordProgressDelta {
        accepted_records: 1,
        ..Default::default()
    };
    let bytes = SourceBackedRecordProgressDelta {
        completed_bytes: 512,
        ..Default::default()
    };

    assert_eq!(
        progress.advanced_at(bytes.clone(), started),
        Some(SourceRecordProgressSnapshot {
            completed_records: 0,
            completed_bytes: 512,
        })
    );
    assert_eq!(
        progress.advanced_at(accepted.clone(), started + Duration::from_millis(500)),
        None
    );
    assert_eq!(
        progress.advanced_at(bytes, started + SOURCE_RECORD_PROGRESS_INTERVAL),
        Some(SourceRecordProgressSnapshot {
            completed_records: 1,
            completed_bytes: 1_024,
        })
    );
    assert_eq!(
        progress.advanced_at(accepted.clone(), started + Duration::from_millis(1_100)),
        None
    );
    assert_eq!(
        progress.flush_at(started + Duration::from_millis(1_100)),
        Some(SourceRecordProgressSnapshot {
            completed_records: 2,
            completed_bytes: 1_024,
        })
    );
    assert_eq!(
        progress.flush_at(started + Duration::from_millis(1_100)),
        None
    );

    let mut next_source = SourceRecordProgress::default();
    assert_eq!(next_source.completed_records, 0);
    assert_eq!(next_source.completed_bytes, 0);
    assert_eq!(
        next_source.advanced_at(accepted, started),
        Some(SourceRecordProgressSnapshot {
            completed_records: 1,
            completed_bytes: 0,
        })
    );
}

#[test]
fn attempt_history_progress_deduplicates_full_session_identity_and_accumulates_counts() {
    let first_session = [0x11; 32];
    let second_session = [0x22; 32];
    let mut progress = AttemptHistoryProgress::default();
    progress.advance(&SourceBackedRecordProgressDelta {
        accepted_records: 4,
        completed_bytes: 1_024,
        session_ids: vec![first_session, second_session, first_session],
        messages: 3,
        tool_calls: 1,
    });
    progress.advance(&SourceBackedRecordProgressDelta {
        accepted_records: 2,
        completed_bytes: 512,
        session_ids: vec![second_session],
        messages: 1,
        tool_calls: 1,
    });

    assert_eq!(
        progress.snapshot(),
        AttemptHistoryProgressSnapshot {
            processed_sessions: 2,
            processed_messages: 4,
            processed_tool_calls: 2,
            processed_bytes: 1_536,
        }
    );
}
