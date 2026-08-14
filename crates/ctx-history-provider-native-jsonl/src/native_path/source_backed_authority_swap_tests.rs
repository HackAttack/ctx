use std::{fs, path::PathBuf};

use ctx_history_core::CaptureProvider;
use ctx_history_jsonl::{JsonlFamilyAdapter, JsonlFamilyLeaf};
use ctx_history_source_io::{
    NON_REGULAR_PROVIDER_SOURCE_REASON, REPARSE_PROVIDER_SOURCE_REASON,
    SYMLINK_PROVIDER_SOURCE_REASON,
};

#[cfg(unix)]
use ctx_history_source_io::test_support_paths::{make_fifo, tempdir};

use super::*;

fn discovered_leaf() -> (
    tempfile::TempDir,
    PathBuf,
    PathBuf,
    JsonlFamilyLeaf<CaptureError>,
) {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let ancestor = temp.path().join("authority");
    let root = ancestor.join("transcripts");
    let leaf = root.join("session.jsonl");
    fs::create_dir_all(&root).unwrap();
    fs::write(&leaf, b"{\"type\":\"message\"}\n").unwrap();
    let adapter = super::super::windsurf_source_backed_adapter::<
        crate::test_support::NativeJsonlTestRuntime,
    >();
    assert_eq!(adapter.provider(), CaptureProvider::Windsurf);
    let inventory = adapter.discover(&root).unwrap();
    assert_eq!(inventory.leaves().len(), 1);
    let retained = inventory.leaves()[0].clone();
    (temp, ancestor, root, retained)
}

#[test]
fn shared_native_jsonl_rejects_root_swap_after_discovery() {
    let (_temp, _ancestor, root, retained) = discovered_leaf();
    let displaced = root.with_file_name("transcripts-displaced");
    fs::rename(&root, &displaced).unwrap();
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("session.jsonl"), b"{\"replacement\":true}\n").unwrap();

    assert!(retained.open_verified().is_err());
}

#[test]
fn shared_native_jsonl_rejects_ancestor_swap_after_discovery() {
    let (temp, ancestor, root, retained) = discovered_leaf();
    let displaced = temp.path().join("authority-displaced");
    fs::rename(&ancestor, &displaced).unwrap();
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("session.jsonl"), b"{\"replacement\":true}\n").unwrap();

    assert!(retained.open_verified().is_err());
}

#[test]
fn shared_native_jsonl_rejects_leaf_swap_after_discovery() {
    let (_temp, _ancestor, root, retained) = discovered_leaf();
    let leaf = root.join("session.jsonl");
    fs::rename(&leaf, root.join("session-displaced.jsonl")).unwrap();
    fs::write(&leaf, b"{\"replacement\":true}\n").unwrap();

    assert!(retained.open_verified().is_err());
}

fn membership_rejection(path: &str, reason: &'static str) -> CaptureError {
    CaptureError::InvalidProviderTranscriptPath {
        path: PathBuf::from(path),
        reason,
    }
}

#[test]
fn shared_native_jsonl_skips_only_unselected_membership_rejections() {
    for reason in [
        SYMLINK_PROVIDER_SOURCE_REASON,
        REPARSE_PROVIDER_SOURCE_REASON,
        NON_REGULAR_PROVIDER_SOURCE_REASON,
    ] {
        let error = membership_rejection("unselected-sibling", reason);
        assert!(membership_open_error_is_ignorable(false, &error));

        let error = membership_rejection("selected-transcript.jsonl", reason);
        assert!(!membership_open_error_is_ignorable(true, &error));
    }
}

#[cfg(unix)]
#[test]
fn shared_native_jsonl_skips_unselected_link_and_special_file_siblings() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().unwrap();
    let outside = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("transcripts");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("session.jsonl"), b"{\"type\":\"message\"}\n").unwrap();
    fs::write(
        outside.path().join("outside.jsonl"),
        b"{\"outside\":true}\n",
    )
    .unwrap();
    symlink(outside.path(), root.join("unselected-link")).unwrap();
    make_fifo(&root.join("unselected.fifo")).unwrap();

    let adapter = super::super::windsurf_source_backed_adapter::<
        crate::test_support::NativeJsonlTestRuntime,
    >();
    let inventory = adapter.discover(&root).unwrap();

    assert_eq!(inventory.leaves().len(), 1);
    assert_eq!(
        inventory.leaves()[0].source_path(),
        root.join("session.jsonl")
    );
}

#[cfg(unix)]
#[test]
fn shared_native_jsonl_rejects_selected_link_and_special_file_transcripts() {
    use std::os::unix::fs::symlink;

    let outside = crate::test_support_paths::tempdir().unwrap();
    let adapter = super::super::windsurf_source_backed_adapter::<
        crate::test_support::NativeJsonlTestRuntime,
    >();

    let linked = crate::test_support_paths::tempdir().unwrap();
    fs::write(
        outside.path().join("outside.jsonl"),
        b"{\"outside\":true}\n",
    )
    .unwrap();
    symlink(
        outside.path().join("outside.jsonl"),
        linked.path().join("linked.jsonl"),
    )
    .unwrap();
    let error = adapter.discover(linked.path()).unwrap_err();
    assert!(matches!(
        error,
        CaptureError::InvalidProviderTranscriptPath { reason, .. }
            if reason == SYMLINK_PROVIDER_SOURCE_REASON
    ));

    let special = tempdir().unwrap();
    make_fifo(&special.path().join("fifo.jsonl")).unwrap();
    let error = adapter.discover(special.path()).unwrap_err();
    assert!(matches!(
        error,
        CaptureError::InvalidProviderTranscriptPath { reason, .. }
            if reason == NON_REGULAR_PROVIDER_SOURCE_REASON
    ));
}
