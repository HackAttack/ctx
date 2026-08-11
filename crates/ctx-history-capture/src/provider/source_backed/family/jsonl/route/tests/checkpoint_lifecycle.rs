use super::*;

#[test]
fn opaque_provider_checkpoint_and_base_lookup_resume_only_the_certified_suffix() {
    for workers in [1, 8] {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let root = temp.path().join("sessions");
        let index = temp.path().join("index");
        fs::create_dir_all(&root).unwrap();
        let transcripts = (0..workers)
            .map(|index| root.join(format!("checkpoint-{index}.jsonl")))
            .collect::<Vec<_>>();
        for transcript in &transcripts {
            fs::write(transcript, b"{\"message\":\"prefix\"}\n").unwrap();
        }

        let cold = capture_checkpoint_test_generation(&root, &index, workers);
        assert!(provider_checkpoints(&cold)
            .into_iter()
            .all(|checkpoint| checkpoint == Some(TypedKey::U64(1))));

        for transcript in &transcripts {
            OpenOptions::new()
                .append(true)
                .open(transcript)
                .unwrap()
                .write_all(b"{\"message\":\"suffix\"}\n")
                .unwrap();
        }
        let appended = capture_checkpoint_test_generation(&root, &index, workers);
        assert!(provider_checkpoints(&appended)
            .into_iter()
            .all(|checkpoint| checkpoint == Some(TypedKey::U64(2))));
        assert!(appended
            .manifest()
            .sources
            .iter()
            .all(|source| source.counts().complete_records == 2));
    }
}

#[test]
fn family_checkpoint_writes_compact_utf8_and_reads_legacy_bytes() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    let index = temp.path().join("index");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("checkpoint.jsonl"), b"{\"message\":\"prefix\"}\n").unwrap();

    let receipt = capture_checkpoint_test_generation(&root, &index, 1);
    let frontier = receipt.manifest().sources[0].frontier().unwrap();
    let TypedKey::Utf8(json) = frontier.checkpoint() else {
        panic!("new family checkpoint was not compact UTF-8");
    };
    let checkpoint = FamilyCheckpoint::decode_frontier_key(frontier.checkpoint()).unwrap();
    assert_eq!(checkpoint.version, FamilyCheckpoint::VERSION);

    let legacy = TypedKey::bytes(serde_json::to_vec(&checkpoint).unwrap()).unwrap();
    assert_eq!(
        FamilyCheckpoint::decode_frontier_key(&legacy).unwrap(),
        checkpoint
    );
    assert!(
        serde_json::to_vec(frontier.checkpoint()).unwrap().len()
            < serde_json::to_vec(&legacy).unwrap().len()
    );
    assert_eq!(
        serde_json::from_str::<FamilyCheckpoint>(json).unwrap(),
        checkpoint
    );
}

#[test]
fn nonterminal_checkpoint_noops_then_resumes_only_its_uncertified_tail() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    let transcript = root.join("incomplete.jsonl");
    fs::write(
        &transcript,
        b"{\"message\":\"prefix\"}\n{\"message\":\"tail\"",
    )
    .unwrap();
    let writer = GenerationWriter::open(
        temp.path().join("index"),
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 15_000_000,
        },
    )
    .unwrap()
    .into_writer()
    .unwrap();
    let lookup = writer.base_event_identity_lookup();
    let adapter = CheckpointTestAdapter::default();
    let mut worker = JsonlFamilyWorkerContext::default();

    let cold = {
        let inventory = adapter.discover(&root).unwrap();
        let leaf = inventory.leaves().first().unwrap();
        let mut emit = |_event| Ok(());
        let mut output = JsonlLeafOutput::new(&mut emit);
        prepare_leaf(&adapter, leaf, None, &lookup, &mut worker, &mut output).unwrap()
    };
    let cold_inventory = adapter.discover(&root).unwrap();
    let cold_checkpoint = super::super::leaf::decode_checkpoint(
        &adapter,
        cold_inventory.leaves().first().unwrap(),
        &cold.certificate,
    )
    .unwrap();
    assert!(!cold_checkpoint.physical.terminal());
    assert_eq!(cold_checkpoint.physical.next_physical_ordinal(), 1);
    assert_eq!(cold_checkpoint.provider_checkpoint, Some(TypedKey::U64(1)));
    assert_eq!(
        adapter.projection_modes.lock().unwrap().as_slice(),
        [JsonlFamilyProjectionMode::Cold]
    );

    let unchanged = {
        let inventory = adapter.discover(&root).unwrap();
        let leaf = inventory.leaves().first().unwrap();
        let mut events = Vec::new();
        let mut emit = |event| {
            events.push(event);
            Ok(())
        };
        let mut output = JsonlLeafOutput::new(&mut emit);
        let prepared = prepare_leaf(
            &adapter,
            leaf,
            Some(&cold.certificate),
            &lookup,
            &mut worker,
            &mut output,
        )
        .unwrap();
        drop(output);
        assert!(events.is_empty());
        prepared
    };
    assert_eq!(unchanged.certificate, cold.certificate);
    assert!(unchanged.append.is_some());
    assert_eq!(
        adapter.projection_modes.lock().unwrap().as_slice(),
        [JsonlFamilyProjectionMode::Cold],
        "an exactly unchanged incomplete tail must not reconstruct a projector"
    );

    OpenOptions::new()
        .append(true)
        .open(&transcript)
        .unwrap()
        .write_all(b"}\n")
        .unwrap();
    let completed = {
        let inventory = adapter.discover(&root).unwrap();
        let leaf = inventory.leaves().first().unwrap();
        let mut emit = |_event| Ok(());
        let mut output = JsonlLeafOutput::new(&mut emit);
        prepare_leaf(
            &adapter,
            leaf,
            Some(&cold.certificate),
            &lookup,
            &mut worker,
            &mut output,
        )
        .unwrap()
    };
    assert!(completed.append.is_some());
    let completed_inventory = adapter.discover(&root).unwrap();
    let completed_checkpoint = super::super::leaf::decode_checkpoint(
        &adapter,
        completed_inventory.leaves().first().unwrap(),
        &completed.certificate,
    )
    .unwrap();
    assert!(completed_checkpoint.physical.terminal());
    assert_eq!(completed_checkpoint.physical.next_physical_ordinal(), 2);
    assert_eq!(
        completed_checkpoint.provider_checkpoint,
        Some(TypedKey::U64(2))
    );
    assert_eq!(
        adapter.projection_modes.lock().unwrap().as_slice(),
        [
            JsonlFamilyProjectionMode::Cold,
            JsonlFamilyProjectionMode::CertifiedAppend,
        ],
        "tail completion must resume the shared checkpoint instead of replacing the source"
    );
}
