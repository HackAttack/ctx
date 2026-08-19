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
    let root = ancestor.join("session-state");
    let leaf = root.join("copilot-cli-native/events.jsonl");
    fs::create_dir_all(leaf.parent().unwrap()).unwrap();
    fs::write(
        &leaf,
        b"{\"type\":\"session.start\",\"data\":{\"sessionId\":\"authority-swap\"}}\n",
    )
    .unwrap();
    let adapter = super::super::copilot_source_backed_adapter::<
        crate::test_support::NativeJsonlTestRuntime,
    >();
    assert_eq!(adapter.provider(), CaptureProvider::CopilotCli);
    let inventory = adapter.discover(&root).unwrap();
    assert_eq!(inventory.accepted_len(), 1);
    let retained = inventory.accepted_leaves().next().unwrap().clone();
    (temp, ancestor, root, retained)
}

#[test]
fn shared_native_jsonl_rejects_root_swap_after_discovery() {
    let (_temp, _ancestor, root, retained) = discovered_leaf();
    let displaced = root.with_file_name("transcripts-displaced");
    fs::rename(&root, &displaced).unwrap();
    fs::create_dir_all(root.join("copilot-cli-native")).unwrap();
    fs::write(
        root.join("copilot-cli-native/events.jsonl"),
        b"{\"replacement\":true}\n",
    )
    .unwrap();

    assert!(retained.open_verified().is_err());
}

#[test]
fn shared_native_jsonl_rejects_ancestor_swap_after_discovery() {
    let (temp, ancestor, root, retained) = discovered_leaf();
    let displaced = temp.path().join("authority-displaced");
    fs::rename(&ancestor, &displaced).unwrap();
    fs::create_dir_all(root.join("copilot-cli-native")).unwrap();
    fs::write(
        root.join("copilot-cli-native/events.jsonl"),
        b"{\"replacement\":true}\n",
    )
    .unwrap();

    assert!(retained.open_verified().is_err());
}

#[test]
fn shared_native_jsonl_rejects_leaf_swap_after_discovery() {
    let (_temp, _ancestor, root, retained) = discovered_leaf();
    let leaf = root.join("copilot-cli-native/events.jsonl");
    fs::rename(
        &leaf,
        root.join("copilot-cli-native/events-displaced.jsonl"),
    )
    .unwrap();
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
    let root = temp.path().join("session-state");
    fs::create_dir_all(root.join("copilot-cli-native")).unwrap();
    fs::write(
        root.join("copilot-cli-native/events.jsonl"),
        b"{\"type\":\"session.start\",\"data\":{\"sessionId\":\"membership\"}}\n",
    )
    .unwrap();
    fs::write(
        outside.path().join("outside.jsonl"),
        b"{\"outside\":true}\n",
    )
    .unwrap();
    symlink(outside.path(), root.join("unselected-link")).unwrap();
    make_fifo(&root.join("unselected.fifo")).unwrap();

    let adapter = super::super::copilot_source_backed_adapter::<
        crate::test_support::NativeJsonlTestRuntime,
    >();
    let inventory = adapter.discover(&root).unwrap();

    assert_eq!(inventory.accepted_len(), 1);
    assert_eq!(
        inventory.accepted_leaves().next().unwrap().source_path(),
        root.join("copilot-cli-native/events.jsonl")
    );
}

#[cfg(unix)]
#[test]
fn shared_native_jsonl_rejects_selected_link_and_special_file_transcripts() {
    use std::os::unix::fs::symlink;

    let outside = crate::test_support_paths::tempdir().unwrap();
    let adapter = super::super::copilot_source_backed_adapter::<
        crate::test_support::NativeJsonlTestRuntime,
    >();

    let linked = crate::test_support_paths::tempdir().unwrap();
    let linked_session = linked.path().join("copilot-cli-native");
    fs::create_dir_all(&linked_session).unwrap();
    fs::write(
        outside.path().join("outside.jsonl"),
        b"{\"outside\":true}\n",
    )
    .unwrap();
    symlink(
        outside.path().join("outside.jsonl"),
        linked_session.join("events.jsonl"),
    )
    .unwrap();
    let error = adapter.discover(linked.path()).unwrap_err();
    assert!(matches!(
        error,
        CaptureError::InvalidProviderTranscriptPath { reason, .. }
            if reason == SYMLINK_PROVIDER_SOURCE_REASON
    ));

    let special = tempdir().unwrap();
    let special_session = special.path().join("copilot-cli-native");
    fs::create_dir_all(&special_session).unwrap();
    make_fifo(&special_session.join("events.jsonl")).unwrap();
    let error = adapter.discover(special.path()).unwrap_err();
    assert!(matches!(
        error,
        CaptureError::InvalidProviderTranscriptPath { reason, .. }
            if reason == NON_REGULAR_PROVIDER_SOURCE_REASON
    ));
}
