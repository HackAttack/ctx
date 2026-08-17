use super::*;

fn identities() -> SpillVerificationIdentities {
    SpillVerificationIdentities {
        event: CompactIdentity { digest: [7; 32] },
        session: CompactIdentity { digest: [1; 32] },
        parent_session: Some(CompactIdentity { digest: [2; 32] }),
        root_session: Some(CompactIdentity { digest: [3; 32] }),
        session_source_ordinal: 4,
    }
}

#[test]
fn anonymous_file_is_cleaned_up() {
    let cleaned = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let spill = VerificationSpill::create_with_witness(
        [1].into_iter(),
        Some(std::sync::Arc::clone(&cleaned)),
    )
    .unwrap();
    assert!(!cleaned.load(std::sync::atomic::Ordering::SeqCst));
    drop(spill);
    assert!(cleaned.load(std::sync::atomic::Ordering::SeqCst));
}

#[test]
fn layout_limit_rejects_segment_metadata_before_allocation() {
    let one_segment_bytes = std::mem::size_of::<u64>() + std::mem::size_of::<u32>();
    let error = preflight_spill_layout([0, 0].into_iter(), one_segment_bytes).unwrap_err();
    assert!(matches!(
        error,
        IndexError::VerificationScratchLimitExceeded {
            required_bytes,
            maximum_bytes,
        } if required_bytes == (one_segment_bytes * 2) as u64
            && maximum_bytes == one_segment_bytes as u64
    ));
}

#[test]
fn identity_sort_layout_accounts_for_every_retained_run_structure() {
    let expected = std::mem::size_of::<IdentitySortRun>()
        + std::mem::size_of::<usize>()
        + std::mem::size_of::<IdentitySortHeapEntry>();
    assert_eq!(identity_sort_layout_heap_bytes(1).unwrap(), expected);
    #[cfg(target_pointer_width = "64")]
    assert_eq!(expected, 64);
}

#[test]
fn identity_sort_layout_admits_twelve_million_spill_records() {
    let run_count = identity_sort_run_count(12_000_000).unwrap();
    assert!(
        identity_sort_scratch_heap_bytes(12_000_000, run_count).unwrap()
            <= MAX_VERIFICATION_SCRATCH_HEAP_BYTES
    );
}

#[test]
fn scratch_disk_boundary_allows_exact_limit_and_rejects_the_next_byte() {
    let budget = VerificationScratchBudget::with_limits(10, 10);
    let _exact = budget.reserve(10, 0).unwrap();
    assert!(matches!(
        budget.reserve(1, 0),
        Err(IndexError::VerificationScratchLimitExceeded {
            required_bytes: 11,
            maximum_bytes: 10,
        })
    ));
}

#[test]
fn scratch_heap_boundary_allows_exact_limit_and_rejects_the_next_byte() {
    let budget = VerificationScratchBudget::with_limits(10, 10);
    let _exact = budget.reserve(0, 10).unwrap();
    assert!(matches!(
        budget.reserve(0, 1),
        Err(IndexError::VerificationScratchLimitExceeded {
            required_bytes: 11,
            maximum_bytes: 10,
        })
    ));
}

#[test]
fn shared_scratch_budget_admits_twelve_million_record_worst_case_envelope() {
    const DOCUMENTS: u64 = 12_000_000;
    let budget = VerificationScratchBudget::production();
    let run_count = identity_sort_run_count(DOCUMENTS * 2).unwrap();

    let _logical = budget
        .reserve(
            DOCUMENTS * VERIFICATION_SPILL_RECORD_BYTES as u64,
            (std::mem::size_of::<u64>() + std::mem::size_of::<u32>()) as u64,
        )
        .unwrap();
    let _changed = budget
        .reserve(DOCUMENTS * IDENTITY_SPILL_RECORD_BYTES as u64, 0)
        .unwrap();
    let _retired = budget
        .reserve(DOCUMENTS * IDENTITY_SPILL_RECORD_BYTES as u64, 0)
        .unwrap();
    let _affected = budget
        .reserve(DOCUMENTS * 2 * COMPACT_IDENTITY_BYTES as u64, 0)
        .unwrap();
    let _affected_sort = budget
        .reserve(
            DOCUMENTS * 2 * COMPACT_IDENTITY_BYTES as u64,
            identity_sort_scratch_heap_bytes(DOCUMENTS * 2, run_count).unwrap(),
        )
        .unwrap();
}

#[test]
fn logical_layout_has_no_fixed_four_million_slot_ceiling() {
    const FIRST_SLOT_BEYOND_OLD_LIMIT: u32 = 4_036_624;

    let spill = VerificationSpill::create([FIRST_SLOT_BEYOND_OLD_LIMIT].into_iter()).unwrap();
    let final_doc_id = FIRST_SLOT_BEYOND_OLD_LIMIT - 1;
    let mut writer = spill
        .segment_range_writer(
            0,
            final_doc_id,
            FIRST_SLOT_BEYOND_OLD_LIMIT,
            FIRST_SLOT_BEYOND_OLD_LIMIT,
        )
        .unwrap();
    writer
        .write_record(final_doc_id, identities(), ProjectionAccumulator::default())
        .unwrap();
    writer.finish().unwrap();

    assert_eq!(
        spill.logical_bytes(),
        u64::from(FIRST_SLOT_BEYOND_OLD_LIMIT) * VERIFICATION_SPILL_RECORD_BYTES as u64
    );
    assert_eq!(
        spill
            .record(DocAddress::new(0, final_doc_id), "test")
            .unwrap()
            .session
            .digest,
        [1; 32]
    );
}

#[test]
fn projection_accumulator_preserves_multiset_addition_modulo_256_bits() {
    let first = [0xff; QUERY_PROJECTION_ACCUMULATOR_BYTES];
    let second = [1; QUERY_PROJECTION_ACCUMULATOR_BYTES];
    let mut accumulator = ProjectionAccumulator::default();
    accumulator.subtract(&first);
    accumulator.subtract(&second);
    accumulator.add(&second);
    accumulator.add(&first);
    assert!(accumulator.is_zero());
}

#[test]
fn contiguous_projection_state_roundtrips_separately_from_identities() {
    let digest = [9; QUERY_PROJECTION_ACCUMULATOR_BYTES];
    let mut expected = ProjectionAccumulator::default();
    expected.subtract(&digest);
    let spill = VerificationSpill::create([1].into_iter()).unwrap();
    let mut writer = spill.segment_writer(0, 1).unwrap();
    writer.write_record(0, identities(), expected).unwrap();
    writer.finish().unwrap();

    let stored = spill.record(DocAddress::new(0, 0), "test").unwrap();
    assert_eq!(stored.session.digest, [1; 32]);
    let mut projections = spill.load_projection_deltas().unwrap();
    assert!(!projections.is_complete(DocAddress::new(0, 0)).unwrap());
    projections
        .accumulate(DocAddress::new(0, 0), &digest)
        .unwrap();
    assert!(projections.is_complete(DocAddress::new(0, 0)).unwrap());
}

#[test]
fn disjoint_segment_ranges_write_exact_positional_records() {
    let spill = VerificationSpill::create([4].into_iter()).unwrap();
    std::thread::scope(|scope| {
        let first = scope.spawn(|| {
            let mut writer = spill.segment_range_writer(0, 0, 2, 4).unwrap();
            writer
                .write_record(0, identities(), ProjectionAccumulator::default())
                .unwrap();
            writer.write_deleted(1).unwrap();
            writer.finish().unwrap();
        });
        let second = scope.spawn(|| {
            let mut writer = spill.segment_range_writer(0, 2, 4, 4).unwrap();
            writer.write_deleted(2).unwrap();
            writer
                .write_record(3, identities(), ProjectionAccumulator::default())
                .unwrap();
            writer.finish().unwrap();
        });
        first.join().unwrap();
        second.join().unwrap();
    });

    assert_eq!(
        spill
            .record(DocAddress::new(0, 0), "test")
            .unwrap()
            .session
            .digest,
        [1; 32]
    );
    assert_eq!(
        spill
            .record(DocAddress::new(0, 3), "test")
            .unwrap()
            .root_session
            .unwrap()
            .digest,
        [3; 32]
    );
}
