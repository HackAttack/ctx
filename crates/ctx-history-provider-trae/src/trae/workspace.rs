use std::path::Path;

use crate::MAX_PROVIDER_JSONL_LINE_BYTES;
use ctx_history_provider_runtime::source_io::read_json_file_limited;

use super::normalization::trae_first_present_string_field;

pub(super) fn trae_workspace_id(path: &Path) -> String {
    path.parent()
        .and_then(|parent| parent.file_name())
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or("state-vscdb")
        .to_owned()
}

pub(super) fn trae_workspace_folder(path: &Path) -> Option<String> {
    let workspace_json = path.parent()?.join("workspace.json");
    let value = read_json_file_limited(
        &workspace_json,
        MAX_PROVIDER_JSONL_LINE_BYTES,
        "Trae workspace.json",
    )
    .ok()?;
    trae_first_present_string_field(&value, &["folder", "workspace", "path"])
        .map(|folder| trae_workspace_folder_label(&folder))
}

fn trae_workspace_folder_label(folder: &str) -> String {
    let Some(path) = folder.strip_prefix("file://") else {
        return folder.to_owned();
    };
    percent_decode_uri_path(path)
}

fn percent_decode_uri_path(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hi = (bytes[index + 1] as char).to_digit(16);
            let lo = (bytes[index + 2] as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push(((hi << 4) | lo) as u8);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| value.to_owned())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::trae_workspace_folder;

    #[test]
    fn blank_folder_alias_suppresses_later_workspace_and_path_aliases() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let source = temp.path().join("state.vscdb");
        fs::write(
            temp.path().join("workspace.json"),
            r#"{"folder":"  ","workspace":"file:///later/workspace","path":"/later/path"}"#,
        )
        .unwrap();

        assert_eq!(trae_workspace_folder(&source), None);
    }
}
