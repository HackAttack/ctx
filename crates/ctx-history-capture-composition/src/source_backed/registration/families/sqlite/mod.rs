use super::*;

mod logical;
mod other;

pub use logical::*;
pub use other::*;

pub(super) fn register_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    data_root: &Path,
) -> SourceBackedCoordinatorResult<()> {
    match source.provider {
        CaptureProvider::Zed => logical::register_zed_route(registry, source, selection, data_root),
        CaptureProvider::KiroCli => {
            other::register_kiro_source_backed_route(registry, source, selection, data_root)
        }
        CaptureProvider::Firebender => {
            other::register_firebender_source_backed_route(registry, source, selection, data_root)
        }
        CaptureProvider::DeepAgents => {
            logical::register_deepagents_route(registry, source, selection, data_root)
        }
        CaptureProvider::ForgeCode => {
            logical::register_forgecode_selected_route(registry, source, selection, data_root)
        }
        CaptureProvider::OpenCode | CaptureProvider::Kilo | CaptureProvider::MiMoCode => {
            logical::register_opencode_family_route(registry, source, selection, data_root)
        }
        provider => Err(invalid_route(
            provider,
            "this provider is not registered by the SQLite route family",
        )),
    }
}
