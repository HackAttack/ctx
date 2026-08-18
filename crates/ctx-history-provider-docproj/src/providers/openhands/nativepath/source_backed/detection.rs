use std::path::{Component, Path, PathBuf};

use super::{OpenHandsSourceBackedErrorV2, OpenHandsSourceBackedResultV2};

pub(super) struct OpenHandsEventPath {
    pub(super) conversation_id: String,
    pub(super) conversation_root: PathBuf,
}

/// Recognizes the two bounded OpenHands event-file layouts.
///
/// Legacy V1 permits JSON leaves below `v1_conversations/<id>/` for released
/// compatibility. Current CLI history is intentionally narrower and admits
/// only `conversations/<id>/events/event-*.json`.
pub(super) fn openhands_event_path(
    path: &Path,
) -> OpenHandsSourceBackedResultV2<Option<OpenHandsEventPath>> {
    let components = path.components().collect::<Vec<_>>();
    for index in (0..components.len()).rev() {
        let component = components[index].as_os_str();
        let legacy = component == "v1_conversations"
            && components.len() >= index.saturating_add(3)
            && path.extension().and_then(|extension| extension.to_str()) == Some("json");
        let current = component == "conversations"
            && components.len() == index.saturating_add(4)
            && components
                .get(index.saturating_add(2))
                .is_some_and(|component| component.as_os_str() == "events")
            && current_cli_event_file(path);
        if legacy || current {
            return Ok(Some(OpenHandsEventPath {
                conversation_id: conversation_id(path, &components, index)?,
                conversation_root: conversation_root(&components, index),
            }));
        }
    }
    Ok(None)
}

fn conversation_id(
    path: &Path,
    components: &[Component<'_>],
    layout_index: usize,
) -> OpenHandsSourceBackedResultV2<String> {
    components
        .get(layout_index.saturating_add(1))
        .and_then(|component| match component {
            Component::Normal(value) => value.to_str(),
            _ => None,
        })
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(
            || OpenHandsSourceBackedErrorV2::MissingConversationCoordinate {
                path: path.to_path_buf(),
            },
        )
}

fn conversation_root(components: &[Component<'_>], layout_index: usize) -> PathBuf {
    let mut root = PathBuf::new();
    for component in components.iter().take(layout_index.saturating_add(2)) {
        root.push(component.as_os_str());
    }
    root
}

fn current_cli_event_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("event-") && name.ends_with(".json"))
}
