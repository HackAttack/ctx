use super::*;
use ctx_history_provider_docproj::OPENHANDS_FILE_EVENTS_SOURCE_FORMAT;

pub(super) fn openhands_automatic_retirement(
    source: &ProviderSource,
    selection: SourceBackedRouteSelection,
    current_root: Option<&Path>,
) -> SourceBackedCoordinatorResult<Option<(SourceRouteIdentity, SourceRouteIdentity)>> {
    if selection != SourceBackedRouteSelection::Automatic {
        return Ok(None);
    }
    let replacement = automatic_source_backed_route_identity(source)?;
    let mut retired = source.clone();
    match source.source_format {
        OPENHANDS_CURRENT_CLI_SOURCE_FORMAT => {
            retired.source_format = OPENHANDS_FILE_EVENTS_SOURCE_FORMAT;
        }
        OPENHANDS_FILE_EVENTS_SOURCE_FORMAT => {
            let Some(current_root) = current_root else {
                return Ok(None);
            };
            retired.source_format = OPENHANDS_CURRENT_CLI_SOURCE_FORMAT;
            retired.path = current_root.to_path_buf();
        }
        _ => return Ok(None),
    }
    Ok(Some((
        replacement,
        automatic_source_backed_route_identity(&retired)?,
    )))
}
