use super::*;

fn generation(root: &Path, digit: char) -> PathBuf {
    root.join(INDEX_GENERATIONS_DIRECTORY)
        .join(format!("generation-{}", digit.to_string().repeat(32)))
}

fn pointer(digit: char) -> ActiveGenerationPointer {
    let digit = digit.to_string();
    ActiveGenerationPointer::new(
        GenerationSlot::new(
            digit.repeat(64),
            format!("generation-{}", digit.repeat(32)),
            digit.repeat(64),
        )
        .unwrap(),
        None,
    )
    .unwrap()
}

#[test]
fn managed_link_creation_and_cleanup_are_retryable_stable_snapshots() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let active = generation(root, '1');
    let candidate = generation(root, '2');
    fs::create_dir_all(&active).unwrap();
    fs::create_dir_all(&candidate).unwrap();
    let relative = Path::new("payload.bin");
    let active_path = active.join(relative);
    let candidate_path = candidate.join(relative);
    fs::write(&active_path, b"immutable payload").unwrap();

    let (file, before_link) = open_artifact_file_snapshot(&active_path).unwrap().unwrap();
    fs::hard_link(&active_path, &candidate_path).unwrap();
    assert!(matches!(
        stable_artifact_link_snapshot(root, &active_path, relative, &file, &before_link, None,)
            .unwrap(),
        ArtifactLinkSnapshot::Retry
    ));
    drop(file);

    let (_, linked) = open_artifact(root, &active, relative, None).unwrap();
    assert_eq!(linked.identity.link_count(), 2);
    let (file, before_unlink) = open_artifact_file_snapshot(&active_path).unwrap().unwrap();
    fs::remove_file(&candidate_path).unwrap();
    assert!(matches!(
        stable_artifact_link_snapshot(root, &active_path, relative, &file, &before_unlink, None,)
            .unwrap(),
        ArtifactLinkSnapshot::Retry
    ));
    drop(file);

    let (_, unlinked) = open_artifact(root, &active, relative, None).unwrap();
    assert_eq!(unlinked.identity.link_count(), 1);
    assert!(linked.same_payload_identity_changed(&unlinked));
}

#[test]
fn generation_disappearing_during_alias_scan_is_retryable() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let active = generation(root, '1');
    let candidate = generation(root, '2');
    fs::create_dir_all(&active).unwrap();
    fs::create_dir_all(&candidate).unwrap();
    let relative = Path::new("payload.bin");
    let active_path = active.join(relative);
    let candidate_path = candidate.join(relative);
    fs::write(&active_path, b"immutable payload").unwrap();
    fs::hard_link(&active_path, &candidate_path).unwrap();
    let (file, linked) = open_artifact_file_snapshot(&active_path).unwrap().unwrap();

    let candidate_for_hook = candidate.clone();
    let candidate_path_for_hook = candidate_path.clone();
    let _hook = AliasEntryTestHookGuard::install(move |entry_path| {
        if entry_path == candidate_for_hook {
            fs::remove_file(&candidate_path_for_hook).unwrap();
            fs::remove_dir(&candidate_for_hook).unwrap();
        }
    });

    assert!(matches!(
        stable_artifact_link_snapshot(root, &active_path, relative, &file, &linked, None,).unwrap(),
        ArtifactLinkSnapshot::Retry
    ));
}

#[test]
fn stale_directory_entry_errors_are_retryable_but_io_errors_are_not() {
    assert!(retryable_alias_snapshot_error(&std::io::Error::from(
        std::io::ErrorKind::NotFound,
    )));
    assert!(!retryable_alias_snapshot_error(&std::io::Error::from(
        std::io::ErrorKind::PermissionDenied,
    )));
    #[cfg(unix)]
    assert!(retryable_alias_snapshot_error(
        &std::io::Error::from_raw_os_error(libc::ESTALE)
    ));
}

#[test]
fn pointer_replacement_during_control_capture_is_concurrent_not_corruption() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let first = pointer('1');
    let second = pointer('2');
    let target = root.join("active-generation.json");
    fs::write(&target, serde_json::to_vec(&first).unwrap()).unwrap();
    let directory = DurableMmapDirectory::open(root).unwrap();
    let target_for_hook = target.clone();
    let second_bytes = serde_json::to_vec(&second).unwrap();
    let mut replaced = false;
    let hook = RegularFileIdentityTestHookGuard::install(move |path| {
        if path == target_for_hook && !replaced {
            directory
                .atomic_write(Path::new("active-generation.json"), &second_bytes)
                .unwrap();
            replaced = true;
        }
    });

    assert!(matches!(
        capture_pointer_bound_single_link_control(root, &first, &target),
        Err(IndexError::ConcurrentGenerationChange)
    ));
    drop(hook);
    assert_eq!(load_current_pointer(root).unwrap(), second);

    let directory = DurableMmapDirectory::open(root).unwrap();
    let target_for_hook = target.clone();
    let second_bytes = serde_json::to_vec(&second).unwrap();
    let mut rewritten = false;
    let hook = RegularFileIdentityTestHookGuard::install(move |path| {
        if path == target_for_hook && !rewritten {
            directory
                .atomic_write(Path::new("active-generation.json"), &second_bytes)
                .unwrap();
            rewritten = true;
        }
    });
    assert!(capture_pointer_bound_single_link_control(root, &second, &target).is_ok());
    drop(hook);

    fs::hard_link(&target, root.join("unmanaged-pointer-hardlink")).unwrap();
    assert!(matches!(
        capture_pointer_bound_single_link_control(root, &second, &target),
        Err(IndexError::ChecksumMismatch)
    ));
}

#[test]
fn stable_unmanaged_hardlink_remains_checksum_mismatch() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let active = generation(root, '1');
    fs::create_dir_all(&active).unwrap();
    let relative = Path::new("payload.bin");
    let active_path = active.join(relative);
    fs::write(&active_path, b"immutable payload").unwrap();
    fs::hard_link(&active_path, root.join("unmanaged-hardlink")).unwrap();

    assert!(matches!(
        open_artifact(root, &active, relative, None),
        Err(IndexError::ChecksumMismatch)
    ));
}
