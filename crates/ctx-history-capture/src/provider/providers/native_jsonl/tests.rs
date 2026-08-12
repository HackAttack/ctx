use std::{ffi::OsStr, fs, path::Path};

use crate::test_support_paths::tempdir;
use ctx_history_core::CaptureProvider;

use super::visit_native_jsonl_files;

#[test]
fn tree_visitation_is_sorted_by_durable_filename_bytes() {
    fn visited_agents(root: &Path, creation_order: &[&str]) -> Vec<String> {
        for agent in creation_order {
            let directory = root.join("sessions/work/session/agents").join(agent);
            fs::create_dir_all(&directory).unwrap();
            fs::write(directory.join("wire.jsonl"), b"\n").unwrap();
        }

        let mut visited = Vec::new();
        visit_native_jsonl_files(root, CaptureProvider::KimiCodeCli, &mut |path| {
            visited.push(
                path.parent()
                    .and_then(Path::file_name)
                    .and_then(OsStr::to_str)
                    .unwrap()
                    .to_owned(),
            );
            Ok(())
        })
        .unwrap();
        visited
    }

    let first = tempdir().unwrap();
    let second = tempdir().unwrap();
    let expected = vec!["agent-1".to_owned(), "main".to_owned()];
    assert_eq!(visited_agents(first.path(), &["main", "agent-1"]), expected);
    assert_eq!(
        visited_agents(second.path(), &["agent-1", "main"]),
        expected
    );
}

#[test]
fn antigravity_dialect_prefers_the_full_transcript_sibling() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("transcript.jsonl"), b"short\n").unwrap();
    fs::write(root.join("transcript_full.jsonl"), b"full\n").unwrap();

    let mut visited = Vec::new();
    let count = visit_native_jsonl_files(&root, CaptureProvider::Antigravity, &mut |path| {
        visited.push(path.file_name().unwrap().to_owned());
        Ok(())
    })
    .unwrap();

    assert_eq!(count, 1);
    assert_eq!(visited, [OsStr::new("transcript_full.jsonl")]);
}
