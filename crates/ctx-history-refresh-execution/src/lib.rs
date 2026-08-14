mod catalog_witness;
mod current_state;
mod execution;
mod explicit_source_catalog;
mod metadata;
mod observation;
mod receipt;
mod receipt_parse;
mod registry_issues;
mod route_result;
#[cfg(test)]
mod tests;
mod types;

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fmt,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration as StdDuration, Instant as StdInstant},
};

use anyhow::{anyhow, bail, Context, Result};
use ctx_history_capture::{
    automatic_source_backed_route_identity, build_automatic_source_backed_registry_from_report,
    discover_provider_sources_with_context_and_work_budget, source_backed_refresh_work_budget,
    source_backed_refresh_writer_options, validate_provider_source_roots_outside_data_root,
    DiscoveryContext, SourceBackedAutomaticRegistryIssue, SourceBackedAutomaticUnavailableReason,
    SourceBackedCoordinatorError,
    SourceBackedDetailedRefreshProgress as CaptureSourceBackedDetailedRefreshProgress,
    SourceBackedFailedRoute, SourceBackedFailedRouteOutcome, SourceBackedLogicalSourceFailures,
    SourceBackedProviderRegistry, SourceBackedRecordRejections, SourceBackedRouteError,
    SourceBackedRouteErrorKind, SourceBackedRouteResult, SourceBackedRouteSelection,
    SourceBackedSelectorAuthority, SourceBackedSourceFailureClass, SourceBackedSourceFailures,
    SourceBackedSuccessfulRouteOutcome, MAX_SOURCE_BACKED_ROUTE_CONTROL_BYTES,
};
#[cfg(test)]
use ctx_history_capture_model::DiscoveryIssue;
use ctx_history_capture_model::{
    DiscoveryIssueKind, DiscoveryReport, ProviderSource, ProviderSourceStatus,
};
use ctx_history_capture_runtime::{CapturePublicationDisposition, ImmutableCaptureSnapshot};
use ctx_history_core::{CaptureProvider, CertifiedSource, ScannedSourceCounts};
use ctx_history_index::{
    GenerationManifest, GenerationWriter, IndexError, SourceRouteIdentity, VerifiedIndex,
    WriterOptions,
};
use serde_json::{json, Value};

use catalog_witness::reconcile_published_catalog_witness;
use observation::{admitted_route_observations, run_after_capture_scan_before_metadata_hook};
use registry_issues::{
    automatic_registry_route_less_blockers, selected_registry_route_count,
    RouteLessRegistryBlockers,
};
type SourceBackedRefreshOperation = RefreshOperation;

pub use ctx_history_capture::{SourceBackedReconciliationDemand, SourceBackedRefreshScope};
pub use current_state::SourceBackedRefreshCurrent;
#[doc(hidden)]
pub use execution::{
    exclusive_scan_stage_duration, execute_capture_owned_refresh_with,
    refresh_all_provider_sources_route_local,
    refresh_all_provider_sources_route_local_with_worksets,
};
#[cfg(any(test, feature = "test-support"))]
pub use explicit_source_catalog::explicit_source_catalog_authority_for_test;
pub use explicit_source_catalog::{
    explicit_source_for_path, relocate_explicit_source, upsert_explicit_source,
    validate_explicit_relocation_source, ExplicitSourceCatalogAuthority,
    ExplicitSourceCatalogRouteBinding, ExplicitSourceCatalogUpsert,
    ExplicitSourceRelocationAuthority,
};
pub use metadata::{
    verify_generation_query_readiness, GenerationQueryReadiness, SourceBackedPublicationMetadata,
    SOURCE_REFRESH_PUBLICATION_METADATA_VERSION,
};
#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub use observation::install_after_capture_scan_before_metadata_hook_for_test;
pub use receipt::SourceBackedRefreshReceipt;
pub use receipt_parse::{
    is_sha256_identity, optional_generation, parse_zero_source_authority,
    published_refresh_receipt_for_index, published_refresh_receipt_for_recovery,
    required_generation, required_route_results, validate_zero_source_authority,
    zero_source_authority_json,
};
#[doc(hidden)]
pub use registry_issues::{
    automatic_registry_route_failures, reject_blocking_automatic_registry_issues,
};
pub use route_result::{
    source_backed_route_retry_disposition, source_failure_class_is_typed,
    SourceBackedRefreshCatalogRouteOutcome, SourceBackedRefreshRecordRejection,
    SourceBackedRefreshRouteOutcome, SourceBackedRefreshRouteResult,
    SourceBackedRefreshSourceFailure,
};
pub use types::{
    nonzero_duration_micros, PublishedSourceBackedState, PublishedSourceBackedStatePort,
    RefreshOperation, SourceBackedCurrentSourceProgress, SourceBackedCurrentSourceProgressStage,
    SourceBackedExactScanProgress, SourceBackedRefreshCoveredPublication,
    SourceBackedRefreshExecution, SourceBackedRefreshProgressUpdate,
    SourceBackedRefreshPublication, SourceBackedRefreshTimings, SourceBackedRefreshWorkset,
    SourceBackedZeroSourceAuthority,
    SourceBackedZeroSourceAuthorityKind,
};

const SEARCH_DIRECTORY: &str = "search";
const LEXICAL_DIRECTORY: &str = "lexical";
const SOURCE_REFRESH_BUILD_ISSUE_LIMIT: usize = 8;
const SOURCE_REFRESH_TERMINAL_ROUTE_LIMIT: usize = 256;
const SOURCE_REFRESH_RECEIPT_JSON_BUDGET_BYTES: usize = 24 * 1024;
const TERMINAL_COVERAGE_ERROR_CODE: &str = "all_provider_terminal_coverage_unavailable";

#[derive(Debug)]
pub struct ZeroSourcePublicationBlocked {
    detail: String,
}

impl ZeroSourcePublicationBlocked {
    #[doc(hidden)]
    pub fn new(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
        }
    }
}

impl fmt::Display for ZeroSourcePublicationBlocked {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{TERMINAL_COVERAGE_ERROR_CODE}: {}", self.detail)
    }
}

impl std::error::Error for ZeroSourcePublicationBlocked {}

pub fn source_backed_index_root(data_root: &Path) -> PathBuf {
    data_root.join(SEARCH_DIRECTORY).join(LEXICAL_DIRECTORY)
}

pub fn source_backed_watch_catalog(
    data_root: &Path,
    discovery: &DiscoveryContext,
) -> Result<ctx_history_capture::SourceBackedWatchCatalog> {
    let discovery = discovery.clone().with_data_root(data_root);
    let work_budget =
        source_backed_refresh_work_budget(source_backed_refresh_writer_options().indexer_threads);
    let discovery_started = StdInstant::now();
    let report = discover_provider_sources_with_context_and_work_budget(&discovery, work_budget);
    let discovery_duration = discovery_started.elapsed();
    validate_provider_source_roots_outside_data_root(data_root, report.sources.iter())
        .context("validate provider roots before deriving source watch catalog")?;
    let mut build =
        build_automatic_source_backed_registry_from_report(&discovery, data_root, report);
    build.discovery_duration = discovery_duration;
    Ok(build.registry.watch_catalog())
}

#[doc(hidden)]
pub fn source_backed_requested_route_observations(
    catalog: &ctx_history_capture::SourceBackedWatchCatalog,
    requested_routes: &BTreeSet<SourceRouteIdentity>,
) -> BTreeMap<SourceRouteIdentity, Option<String>> {
    requested_routes
        .iter()
        .cloned()
        .map(|route| {
            let observation = catalog.certify_route_observation(&route);
            (route, observation)
        })
        .collect()
}

fn committed_generation_recovery_error(
    recovery: ctx_history_index::CommittedPredecessorMigrationRecovery,
) -> ctx_history_index::IndexError {
    ctx_history_index::IndexError::CommittedGenerationNeedsRecovery {
        generation_id: recovery.generation_id().to_owned(),
        stage: "predecessor migration recovery",
        detail: recovery.detail().to_owned(),
    }
}

fn compact_json(mut value: Value) -> Value {
    prune_null_json(&mut value);
    value
}

fn prune_null_json(value: &mut Value) {
    match value {
        Value::Object(map) => map.retain(|_, nested| {
            prune_null_json(nested);
            !nested.is_null()
        }),
        Value::Array(items) => items.iter_mut().for_each(prune_null_json),
        _ => {}
    }
}

#[doc(hidden)]
pub fn refresh_scope_json(scope: &SourceBackedRefreshScope) -> Value {
    match scope {
        SourceBackedRefreshScope::All => json!({ "kind": "all" }),
        SourceBackedRefreshScope::Exact(routes) => json!({
            "kind": "exact",
            "routes": routes.iter().map(SourceRouteIdentity::as_str).collect::<Vec<_>>(),
        }),
    }
}

#[doc(hidden)]
pub fn refresh_scope_from_json(value: Option<&Value>) -> Result<SourceBackedRefreshScope> {
    let value = value.ok_or_else(|| anyhow!("source refresh recovery scope is missing"))?;
    match value.get("kind").and_then(Value::as_str) {
        Some("all") => Ok(SourceBackedRefreshScope::All),
        Some("exact") => {
            let routes = value
                .get("routes")
                .and_then(Value::as_array)
                .ok_or_else(|| anyhow!("exact source refresh recovery scope has no route list"))?;
            if routes.is_empty() || routes.len() > SOURCE_REFRESH_TERMINAL_ROUTE_LIMIT {
                bail!(
                    "exact source refresh recovery scope must contain 1..={SOURCE_REFRESH_TERMINAL_ROUTE_LIMIT} routes"
                );
            }
            routes
                .iter()
                .map(|route| {
                    let route = route.as_str().ok_or_else(|| {
                        anyhow!("exact source refresh recovery route is not a string")
                    })?;
                    SourceRouteIdentity::from_sha256(route.to_owned()).map_err(Into::into)
                })
                .collect::<Result<BTreeSet<_>>>()
                .map(SourceBackedRefreshScope::Exact)
        }
        Some(kind) => bail!("unknown source refresh recovery scope kind `{kind}`"),
        None => bail!("source refresh recovery scope kind is missing"),
    }
}

pub fn execute_refresh(
    execution: SourceBackedRefreshExecution<'_>,
) -> Result<SourceBackedRefreshPublication> {
    execution::execute_capture_owned_refresh(execution)
}

#[doc(hidden)]
pub fn source_backed_watch_catalog_from_report(
    discovery: &DiscoveryContext,
    report: DiscoveryReport,
    discovery_duration: StdDuration,
    data_root: &Path,
    explicit_source_catalog: Option<&ExplicitSourceCatalogAuthority>,
    published_state: &dyn PublishedSourceBackedStatePort,
) -> Result<ctx_history_capture::SourceBackedWatchCatalog> {
    let merged = execution::build_merged_source_backed_registry(
        discovery,
        report,
        discovery_duration,
        data_root,
        explicit_source_catalog,
        published_state,
    )?;
    Ok(merged.build.registry.watch_catalog())
}
