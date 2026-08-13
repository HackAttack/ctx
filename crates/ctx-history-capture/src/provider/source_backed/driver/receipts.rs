use super::super::*;

mod refresh_control_plane;

#[cfg(test)]
thread_local! {
    static BASE_SOURCE_MANIFEST_VISITS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn reset_base_source_manifest_visits() {
    BASE_SOURCE_MANIFEST_VISITS.with(|visits| visits.set(0));
}

#[cfg(test)]
pub(crate) fn base_source_manifest_visits() -> u64 {
    BASE_SOURCE_MANIFEST_VISITS.with(std::cell::Cell::get)
}

#[cfg(test)]
pub(crate) fn record_base_source_manifest_visit() {
    BASE_SOURCE_MANIFEST_VISITS.with(|visits| visits.set(visits.get().saturating_add(1)));
}

pub use ctx_history_capture_runtime::{
    CompleteInventoryOwner, SourceBackedCertifiedRemoval, SourceBackedCurrentSourceProgress,
    SourceBackedCurrentSourceProgressStage, SourceBackedFailedRoute,
    SourceBackedFailedRouteOutcome, SourceBackedLogicalSourceFailure,
    SourceBackedLogicalSourceFailures, SourceBackedReconciliationDemand,
    SourceBackedRecordCompletion, SourceBackedRecordRejection, SourceBackedRecordRejectionClass,
    SourceBackedRecordRejectionDraft, SourceBackedRecordRejectionDrafts,
    SourceBackedRecordRejections, SourceBackedRefreshScope, SourceBackedRevalidationTarget,
    SourceBackedRouteError, SourceBackedRouteErrorKind, SourceBackedRouteResult,
    SourceBackedRouteRevalidation, SourceBackedRouteWatchTargets, SourceBackedSourceFailureClass,
    SourceBackedSourceFailures, SourceBackedSourceOutcome, SourceOwner,
    MAX_RECORDED_SOURCE_BACKED_FAILURES, MAX_SOURCE_BACKED_FAILURE_DETAIL_BYTES,
    MAX_SOURCE_BACKED_FAILURE_SELECTOR_BYTES, MAX_SOURCE_BACKED_ROUTE_CONTROL_BYTES,
};
use ctx_history_capture_runtime::{
    SourceBackedCoordinatorError as RuntimeSourceBackedCoordinatorError,
    SourceBackedGenerationSink as RuntimeSourceBackedGenerationSink,
    SourceBackedRouteDriver as RuntimeSourceBackedRouteDriver,
};

pub type SourceBackedCoordinatorError = RuntimeSourceBackedCoordinatorError<IndexError>;
pub type SourceBackedCoordinatorResult<T> = Result<T, SourceBackedCoordinatorError>;
pub type SourceBackedGenerationSink<'writer> =
    RuntimeSourceBackedGenerationSink<'writer, IndexCaptureLifecycle>;
pub type SourceBackedRouteDriver =
    RuntimeSourceBackedRouteDriver<IndexCaptureLifecycle, SourceBackedRouteControlExpectation>;

/// Runtime metadata for one selected source route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceBackedRouteMetadata {
    pub source: ProviderSource,
    pub certified_source_format: &'static str,
    pub selection: Option<SourceBackedRouteSelection>,
    pub selector_authority: SourceBackedSelectorAuthority,
    pub unsupported_reason: Option<String>,
    pub route_identity: Option<SourceRouteIdentity>,
    pub watch_target_kind: SourceBackedWatchTargetKind,
}
pub(in super::super) fn source_backed_failed_route_from_route(
    route: &SourceBackedRoute,
    class: SourceBackedSourceFailureClass,
    carried_forward: bool,
    detail: impl AsRef<str>,
) -> SourceBackedCoordinatorResult<SourceBackedFailedRoute> {
    let route_identity = route.metadata.route_identity.clone().ok_or_else(|| {
        SourceBackedCoordinatorError::InvalidRoute {
            provider: route.metadata.source.provider,
            detail: "failed executable route has no route identity".to_owned(),
        }
    })?;
    Ok(SourceBackedFailedRoute::new(
        route_identity,
        source_backed_source_failure_identity(&route.metadata.source)?,
        route.metadata.source.provider,
        class,
        carried_forward,
        route.metadata.source.path.display().to_string(),
        detail,
    ))
}

#[derive(Debug, Clone)]
pub(in super::super) struct HermesRouteRetirement {
    pub(in super::super) route_identity: SourceRouteIdentity,
    pub(in super::super) database_identity: [u8; 32],
}

#[derive(Debug, Clone)]
pub struct SourceBackedRoute {
    pub(in super::super) metadata: SourceBackedRouteMetadata,
    pub(in super::super) driver: Option<SourceBackedRouteDriver>,
    pub(in super::super) certified_missing_paths: Vec<PathBuf>,
    pub(in super::super) retire_after_success: Vec<SourceRouteIdentity>,
    pub(in super::super) hermes_retire_after_success: Vec<HermesRouteRetirement>,
    pub(in super::super) codex_generation_participant: Option<usize>,
}

impl SourceBackedRoute {
    #[cfg(test)]
    pub(in crate::provider) fn explicit_manual_unchecked_for_test(
        source: ProviderSource,
        selector_authority: SourceBackedSelectorAuthority,
        certified_source_format: &'static str,
        watch_target_kind: SourceBackedWatchTargetKind,
        driver: SourceBackedRouteDriver,
    ) -> SourceBackedCoordinatorResult<Self> {
        let route_identity = source_backed_route_identity(
            &source,
            certified_source_format,
            SourceBackedRouteSelection::ExplicitManual,
            selector_authority,
        )?;
        Ok(Self {
            metadata: SourceBackedRouteMetadata {
                source,
                certified_source_format,
                selection: Some(SourceBackedRouteSelection::ExplicitManual),
                selector_authority,
                unsupported_reason: None,
                route_identity: Some(route_identity),
                watch_target_kind,
            },
            driver: Some(driver),
            certified_missing_paths: Vec::new(),
            retire_after_success: Vec::new(),
            hermes_retire_after_success: Vec::new(),
            codex_generation_participant: None,
        })
    }

    pub fn automatic(
        source: ProviderSource,
        selector_authority: SourceBackedSelectorAuthority,
        driver: SourceBackedRouteDriver,
    ) -> SourceBackedCoordinatorResult<Self> {
        let known = validate_executable_route(
            &source,
            SourceBackedRouteSelection::Automatic,
            selector_authority,
        )?;
        let route_identity = automatic_source_backed_route_identity(&source)?;
        Ok(Self {
            metadata: SourceBackedRouteMetadata {
                source,
                certified_source_format: known.certified_source_format,
                selection: Some(SourceBackedRouteSelection::Automatic),
                selector_authority,
                unsupported_reason: None,
                route_identity: Some(route_identity),
                watch_target_kind: known.watch_target_kind,
            },
            driver: Some(driver),
            certified_missing_paths: Vec::new(),
            retire_after_success: Vec::new(),
            hermes_retire_after_success: Vec::new(),
            codex_generation_participant: None,
        })
    }

    pub fn explicit_manual(
        source: ProviderSource,
        selector_authority: SourceBackedSelectorAuthority,
        driver: SourceBackedRouteDriver,
    ) -> SourceBackedCoordinatorResult<Self> {
        let known = validate_executable_route(
            &source,
            SourceBackedRouteSelection::ExplicitManual,
            selector_authority,
        )?;
        let route_identity = source_backed_route_identity(
            &source,
            known.certified_source_format,
            SourceBackedRouteSelection::ExplicitManual,
            selector_authority,
        )?;
        Ok(Self {
            metadata: SourceBackedRouteMetadata {
                source,
                certified_source_format: known.certified_source_format,
                selection: Some(SourceBackedRouteSelection::ExplicitManual),
                selector_authority,
                unsupported_reason: None,
                route_identity: Some(route_identity),
                watch_target_kind: known.watch_target_kind,
            },
            driver: Some(driver),
            certified_missing_paths: Vec::new(),
            retire_after_success: Vec::new(),
            hermes_retire_after_success: Vec::new(),
            codex_generation_participant: None,
        })
    }

    pub fn certified_missing(
        source: ProviderSource,
        selector_authority: SourceBackedSelectorAuthority,
    ) -> SourceBackedCoordinatorResult<Self> {
        let known = validate_executable_route(
            &source,
            SourceBackedRouteSelection::Automatic,
            selector_authority,
        )?;
        let route_identity = automatic_source_backed_route_identity(&source)?;
        let path = source.path.clone();
        Ok(Self {
            metadata: SourceBackedRouteMetadata {
                source,
                certified_source_format: known.certified_source_format,
                selection: Some(SourceBackedRouteSelection::Automatic),
                selector_authority,
                unsupported_reason: None,
                route_identity: Some(route_identity),
                watch_target_kind: known.watch_target_kind,
            },
            driver: None,
            certified_missing_paths: vec![path],
            retire_after_success: Vec::new(),
            hermes_retire_after_success: Vec::new(),
            codex_generation_participant: None,
        })
    }

    pub fn unsupported(source: ProviderSource, reason: impl Into<String>) -> Self {
        let certified_source_format = landed_format_route(source.provider, source.source_format)
            .map_or(source.source_format, |route| route.certified_source_format);
        Self {
            metadata: SourceBackedRouteMetadata {
                source,
                certified_source_format,
                selection: None,
                selector_authority: SourceBackedSelectorAuthority::ExplicitPath,
                unsupported_reason: Some(reason.into()),
                route_identity: None,
                watch_target_kind: SourceBackedWatchTargetKind::Path,
            },
            driver: None,
            certified_missing_paths: Vec::new(),
            retire_after_success: Vec::new(),
            hermes_retire_after_success: Vec::new(),
            codex_generation_participant: None,
        }
    }

    pub fn metadata(&self) -> &SourceBackedRouteMetadata {
        &self.metadata
    }
}

#[derive(Debug, Clone, Default)]
pub struct SourceBackedProviderRegistry {
    pub(in super::super) routes: Vec<SourceBackedRoute>,
    pub(in super::super) codex_generation: Option<Arc<CodexGenerationNormalizationCoordinatorV0>>,
}

impl SourceBackedProviderRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, route: SourceBackedRoute) {
        if let Some(identity) = route.metadata.route_identity.as_ref() {
            if let Some(existing) = self
                .routes
                .iter_mut()
                .find(|existing| existing.metadata.route_identity.as_ref() == Some(identity))
            {
                if existing.driver.is_some() {
                    return;
                }
                if route.driver.is_some() {
                    *existing = route;
                    return;
                }
                existing
                    .certified_missing_paths
                    .extend(route.certified_missing_paths);
                existing.certified_missing_paths.sort();
                existing.certified_missing_paths.dedup();
                return;
            }
        }
        self.routes.push(route);
    }

    /// Binds exact carried base routes to an executable replacement route.
    /// Retirement is applied only after that replacement scans and terminally
    /// revalidates successfully; failed replacements retain the base routes.
    pub fn retire_routes_after_success(
        &mut self,
        replacement: &SourceRouteIdentity,
        retired: impl IntoIterator<Item = SourceRouteIdentity>,
    ) -> SourceBackedCoordinatorResult<()> {
        let route = self
            .routes
            .iter_mut()
            .find(|route| route.metadata.route_identity.as_ref() == Some(replacement))
            .ok_or_else(|| SourceBackedCoordinatorError::InvalidRefreshScope {
                route_id: replacement.as_str().to_owned(),
            })?;
        if route.driver.is_none() {
            return Err(SourceBackedCoordinatorError::InvalidRefreshScope {
                route_id: replacement.as_str().to_owned(),
            });
        }
        route.retire_after_success.extend(retired);
        route.retire_after_success.sort();
        route.retire_after_success.dedup();
        if route
            .retire_after_success
            .binary_search(replacement)
            .is_ok()
        {
            return Err(SourceBackedCoordinatorError::InvalidRefreshScope {
                route_id: replacement.as_str().to_owned(),
            });
        }
        Ok(())
    }

    /// Registers stale automatic Hermes routes as conditional retirement
    /// candidates. A candidate is authorized only when the replacement's
    /// successful control reports the same stable physical database identity.
    pub fn retire_hermes_routes_after_success(
        &mut self,
        replacement: &SourceRouteIdentity,
        retired: impl IntoIterator<Item = (SourceRouteIdentity, [u8; 32])>,
    ) -> SourceBackedCoordinatorResult<()> {
        let route = self
            .routes
            .iter_mut()
            .find(|route| route.metadata.route_identity.as_ref() == Some(replacement))
            .ok_or_else(|| SourceBackedCoordinatorError::InvalidRefreshScope {
                route_id: replacement.as_str().to_owned(),
            })?;
        if route.driver.is_none()
            || route.metadata.source.provider != CaptureProvider::Hermes
            || route.metadata.selection != Some(SourceBackedRouteSelection::Automatic)
        {
            return Err(SourceBackedCoordinatorError::InvalidRefreshScope {
                route_id: replacement.as_str().to_owned(),
            });
        }
        route
            .hermes_retire_after_success
            .extend(
                retired
                    .into_iter()
                    .map(
                        |(route_identity, database_identity)| HermesRouteRetirement {
                            route_identity,
                            database_identity,
                        },
                    ),
            );
        route.hermes_retire_after_success.sort_by(|left, right| {
            left.route_identity
                .cmp(&right.route_identity)
                .then(left.database_identity.cmp(&right.database_identity))
        });
        route.hermes_retire_after_success.dedup_by(|left, right| {
            left.route_identity == right.route_identity
                && left.database_identity == right.database_identity
        });
        if route
            .hermes_retire_after_success
            .iter()
            .any(|candidate| &candidate.route_identity == replacement)
        {
            return Err(SourceBackedCoordinatorError::InvalidRefreshScope {
                route_id: replacement.as_str().to_owned(),
            });
        }
        Ok(())
    }

    pub fn routes(&self) -> impl ExactSizeIterator<Item = &SourceBackedRouteMetadata> {
        self.routes.iter().map(SourceBackedRoute::metadata)
    }

    pub fn executable_route_count(&self) -> usize {
        self.routes
            .iter()
            .filter(|route| route.driver.is_some())
            .count()
    }

    /// Returns whether any executable route selected by this exact refresh can
    /// consume the source-scanner half of the coordinated CPU budget.
    pub fn selected_routes_use_parallel_leaf_workers(
        &self,
        scope: &SourceBackedRefreshScope,
    ) -> bool {
        self.routes.iter().any(|route| {
            route
                .driver
                .as_ref()
                .is_some_and(|driver| driver.uses_parallel_leaf_workers)
                && match scope {
                    SourceBackedRefreshScope::All => true,
                    SourceBackedRefreshScope::Exact(selected) => route
                        .metadata
                        .route_identity
                        .as_ref()
                        .is_some_and(|identity| selected.contains(identity)),
                }
        })
    }

    pub fn unsupported_route_count(&self) -> usize {
        self.routes
            .iter()
            .filter(|route| route.driver.is_none())
            .filter(|route| route.certified_missing_paths.is_empty())
            .count()
    }
}

/// Derives the canonical identity for a source's landed automatic route.
///
/// This intentionally accepts sources that failed registration so callers can
/// match route-local failures to a retained healthy route from the same source.
pub fn automatic_source_backed_route_identity(
    source: &ProviderSource,
) -> SourceBackedCoordinatorResult<SourceRouteIdentity> {
    let known = landed_format_route(source.provider, source.source_format)
        .filter(|route| route.automatic)
        .ok_or_else(|| {
            invalid_route(
                source.provider,
                format!(
                    "source format {:?} has no landed automatic route",
                    source.source_format
                ),
            )
        })?;
    source_backed_route_identity(
        source,
        known.certified_source_format,
        SourceBackedRouteSelection::Automatic,
        known.selector_authority,
    )
}

/// Derives the stable source-scoped failure identity used by refresh receipts
/// and direct unsupported-source diagnostics.
pub fn source_backed_source_failure_identity(
    source: &ProviderSource,
) -> SourceBackedCoordinatorResult<String> {
    let certified_source_format = landed_format_route(source.provider, source.source_format)
        .map_or(source.source_format, |route| route.certified_source_format);
    let mut digest = Sha256::new();
    digest.update(b"ctx.source-failure-identity-v1\0");
    digest.update(source.provider.as_str().as_bytes());
    digest.update([0]);
    digest.update(certified_source_format.as_bytes());
    digest.update([0]);
    let path = source.path.as_os_str().as_encoded_bytes();
    digest.update((path.len() as u64).to_be_bytes());
    digest.update(path);
    Ok(format!("{:x}", digest.finalize()))
}

fn source_backed_route_identity(
    source: &ProviderSource,
    certified_source_format: &str,
    selection: SourceBackedRouteSelection,
    selector_authority: SourceBackedSelectorAuthority,
) -> SourceBackedCoordinatorResult<SourceRouteIdentity> {
    let mut digest = Sha256::new();
    digest.update(b"ctx.source-route-identity-v1\0");
    digest.update(source.provider.as_str().as_bytes());
    digest.update([0]);
    digest.update(certified_source_format.as_bytes());
    digest.update([0]);
    digest.update(match selection {
        SourceBackedRouteSelection::Automatic => b"automatic".as_slice(),
        SourceBackedRouteSelection::ExplicitManual => b"explicit".as_slice(),
    });
    digest.update([0]);
    digest.update(match selector_authority {
        SourceBackedSelectorAuthority::DiscoveredWinner => b"discovered-winner".as_slice(),
        SourceBackedSelectorAuthority::ExplicitPath => b"explicit-path".as_slice(),
        SourceBackedSelectorAuthority::CatalogLineage => b"catalog-lineage".as_slice(),
        SourceBackedSelectorAuthority::ExactCwd => b"exact-cwd".as_slice(),
        SourceBackedSelectorAuthority::NamedSurface => b"named-surface".as_slice(),
        SourceBackedSelectorAuthority::SelectedWithRetainedExplicit => {
            b"selected-with-retained-explicit".as_slice()
        }
    });
    // Discovered-winner routes deliberately keep path-independent identity so
    // moving the selected provider root remains an in-place replacement.
    // Catalog-lineage routes instead represent independently owned catalogs;
    // automatic NanoClaw discovery may therefore register several checkouts.
    if selection == SourceBackedRouteSelection::ExplicitManual
        || selector_authority == SourceBackedSelectorAuthority::CatalogLineage
    {
        let path = source.path.as_os_str().as_encoded_bytes();
        digest.update((path.len() as u64).to_be_bytes());
        digest.update(path);
    } else if source.provider == CaptureProvider::Hermes {
        let profile =
            crate::provider::providers::hermes::source_backed::hermes_automatic_profile_name(
                &source.path,
            )
            .map_err(|error| invalid_route(source.provider, error.to_string()))?;
        if profile != "default" {
            // Hermes discovery intentionally multiplexes independently owned
            // named profiles. Keep the historical default route identity, but
            // give every validated named profile a stable path-independent
            // logical slot so registry de-duplication cannot collapse them.
            digest.update(b"\0hermes-profile\0");
            digest.update((profile.len() as u64).to_be_bytes());
            digest.update(profile.as_bytes());
        }
    }
    index_source_route_identity(SourceRouteIdentity::from_sha256(format!(
        "{:x}",
        digest.finalize()
    )))
    .map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_identity_validation_uses_the_canonical_index_conversion() {
        let error = index_source_route_identity(SourceRouteIdentity::from_sha256("AB".repeat(32)))
            .map_err(SourceBackedCoordinatorError::from)
            .unwrap_err();

        assert!(matches!(
            error,
            SourceBackedCoordinatorError::Index(IndexError::InvalidSourceRouteIdentity)
        ));
    }
}
