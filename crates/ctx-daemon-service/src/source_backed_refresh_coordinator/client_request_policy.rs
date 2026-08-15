use super::*;

pub(super) struct SourceBackedRefreshRequestPolicy<'catalog> {
    pub(super) operation: SourceBackedRefreshOperation,
    pub(super) trigger: SourceBackedRefreshTrigger,
    pub(super) explicit_source_catalog: Option<&'catalog ExplicitSourceCatalogAuthority>,
    pub(super) fresh_after_admitted_snapshot: bool,
    pub(super) allow_daemon_autostart: bool,
}

impl<'catalog> SourceBackedRefreshRequestPolicy<'catalog> {
    pub(super) fn refresh(trigger: SourceBackedRefreshTrigger) -> Self {
        Self {
            operation: SourceBackedRefreshOperation::Refresh,
            trigger,
            explicit_source_catalog: None,
            fresh_after_admitted_snapshot: false,
            allow_daemon_autostart: true,
        }
    }

    pub(super) fn import(
        explicit_source_catalog: Option<&'catalog ExplicitSourceCatalogAuthority>,
        allow_daemon_autostart: bool,
    ) -> Self {
        Self {
            operation: if explicit_source_catalog.is_some() {
                SourceBackedRefreshOperation::Import
            } else {
                SourceBackedRefreshOperation::Refresh
            },
            trigger: SourceBackedRefreshTrigger::Import,
            explicit_source_catalog,
            fresh_after_admitted_snapshot: true,
            allow_daemon_autostart,
        }
    }
}
