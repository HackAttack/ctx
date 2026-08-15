use super::*;
use ctx_history_capture::{
    SourceBackedCurrentSourceProgress as CaptureSourceBackedCurrentSourceProgress,
    SourceBackedReconciliationDemand, SourceBackedRefreshScope,
};

/// Maximum exact filesystem members admitted for one route before the route
/// conservatively falls back to exhaustive reconciliation.
pub const SOURCE_BACKED_REFRESH_MEMBER_LIMIT: usize = 256;

/// Provider-neutral physical work admitted for one selected route.
#[derive(Debug, Clone, Eq, PartialEq, Default)]
pub enum SourceBackedRefreshWorkset {
    #[default]
    Exhaustive,
    Members(BTreeSet<PathBuf>),
}

impl SourceBackedRefreshWorkset {
    pub fn members(members: impl IntoIterator<Item = PathBuf>) -> Self {
        let members = members.into_iter().collect::<BTreeSet<_>>();
        if members.is_empty() || members.len() > SOURCE_BACKED_REFRESH_MEMBER_LIMIT {
            Self::Exhaustive
        } else {
            Self::Members(members)
        }
    }

    pub fn merge(&mut self, other: Self) {
        match (self, other) {
            (Self::Exhaustive, _) => {}
            (current @ Self::Members(_), Self::Exhaustive) => *current = Self::Exhaustive,
            (current @ Self::Members(_), Self::Members(additional)) => {
                let Self::Members(members) = current else {
                    return;
                };
                members.extend(additional);
                if members.len() > SOURCE_BACKED_REFRESH_MEMBER_LIMIT {
                    *current = Self::Exhaustive;
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum SourceBackedCurrentSourceProgressStage {
    SourceFamilyCopy,
    OnlineBackup,
    LogicalFingerprint,
    LogicalScan,
}

impl SourceBackedCurrentSourceProgressStage {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SourceFamilyCopy => "source_family_copy",
            Self::OnlineBackup => "online_backup",
            Self::LogicalFingerprint => "logical_fingerprint",
            Self::LogicalScan => "logical_scan",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "source_family_copy" => Some(Self::SourceFamilyCopy),
            "online_backup" => Some(Self::OnlineBackup),
            "logical_fingerprint" => Some(Self::LogicalFingerprint),
            "logical_scan" => Some(Self::LogicalScan),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct SourceBackedCurrentSourceProgress {
    pub stage: SourceBackedCurrentSourceProgressStage,
    pub snapshot_pages_completed: Option<u64>,
    pub snapshot_pages_total: Option<u64>,
    pub snapshot_bytes_completed: Option<u64>,
    pub snapshot_bytes_total: Option<u64>,
    pub logical_rows_scanned: Option<u64>,
    pub logical_certified_bytes: Option<u64>,
}

impl SourceBackedCurrentSourceProgress {
    pub fn to_json(self) -> Value {
        let mut value = json!({
            "stage": self.stage.as_str(),
            "snapshot_pages_completed": self.snapshot_pages_completed,
            "snapshot_pages_total": self.snapshot_pages_total,
            "snapshot_bytes_completed": self.snapshot_bytes_completed,
            "snapshot_bytes_total": self.snapshot_bytes_total,
            "logical_rows_scanned": self.logical_rows_scanned,
            "logical_certified_bytes": self.logical_certified_bytes,
        });
        if let Value::Object(fields) = &mut value {
            fields.retain(|_, value| !value.is_null());
        }
        value
    }

    #[doc(hidden)]
    pub fn from_json(value: &Value) -> Result<Self> {
        let fields = value.as_object().ok_or_else(|| {
            anyhow!("daemon source refresh current-source progress is not an object")
        })?;
        let stage = fields
            .get("stage")
            .and_then(Value::as_str)
            .and_then(SourceBackedCurrentSourceProgressStage::parse)
            .ok_or_else(|| {
                anyhow!("daemon source refresh current-source progress has an invalid stage")
            })?;
        Ok(Self {
            stage,
            snapshot_pages_completed: optional_progress_u64(fields, "snapshot_pages_completed")?,
            snapshot_pages_total: optional_progress_u64(fields, "snapshot_pages_total")?,
            snapshot_bytes_completed: optional_progress_u64(fields, "snapshot_bytes_completed")?,
            snapshot_bytes_total: optional_progress_u64(fields, "snapshot_bytes_total")?,
            logical_rows_scanned: optional_progress_u64(fields, "logical_rows_scanned")?,
            logical_certified_bytes: optional_progress_u64(fields, "logical_certified_bytes")?,
        })
    }

    pub(crate) fn from_capture(progress: CaptureSourceBackedCurrentSourceProgress) -> Self {
        Self {
            stage: match progress.stage {
                ctx_history_capture::SourceBackedCurrentSourceProgressStage::SourceFamilyCopy => {
                    SourceBackedCurrentSourceProgressStage::SourceFamilyCopy
                }
                ctx_history_capture::SourceBackedCurrentSourceProgressStage::OnlineBackup => {
                    SourceBackedCurrentSourceProgressStage::OnlineBackup
                }
                ctx_history_capture::SourceBackedCurrentSourceProgressStage::LogicalFingerprint => {
                    SourceBackedCurrentSourceProgressStage::LogicalFingerprint
                }
                ctx_history_capture::SourceBackedCurrentSourceProgressStage::LogicalScan => {
                    SourceBackedCurrentSourceProgressStage::LogicalScan
                }
            },
            snapshot_pages_completed: progress.snapshot_pages_completed,
            snapshot_pages_total: progress.snapshot_pages_total,
            snapshot_bytes_completed: progress.snapshot_bytes_completed,
            snapshot_bytes_total: progress.snapshot_bytes_total,
            logical_rows_scanned: progress.logical_rows_scanned,
            logical_certified_bytes: progress.logical_certified_bytes,
        }
    }
}

fn optional_progress_u64(
    fields: &serde_json::Map<String, Value>,
    field: &str,
) -> Result<Option<u64>> {
    fields
        .get(field)
        .map(|value| {
            value
                .as_u64()
                .ok_or_else(|| anyhow!("daemon source refresh progress {field} is invalid"))
        })
        .transpose()
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum RefreshOperation {
    Refresh,
    Import,
}

impl RefreshOperation {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Refresh => "refresh",
            Self::Import => "import",
        }
    }

    #[doc(hidden)]
    pub fn from_request_json(request: &Value) -> Result<Self> {
        request
            .get("operation")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("daemon source refresh request operation is missing"))
            .and_then(str::parse)
    }
}

impl std::str::FromStr for RefreshOperation {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "refresh" => Ok(Self::Refresh),
            "import" => Ok(Self::Import),
            operation => Err(anyhow!("invalid source refresh operation `{operation}`")),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub struct SourceBackedRefreshTimings {
    pub discovery_us: u64,
    pub scan_stage_us: u64,
    pub commit_us: u64,
}

impl SourceBackedRefreshTimings {
    #[doc(hidden)]
    pub fn to_json(self) -> Value {
        json!({
            "discovery": self.discovery_us,
            "scan_stage": self.scan_stage_us,
            "commit": self.commit_us,
        })
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum SourceBackedZeroSourceAuthorityKind {
    CompleteEmptyInventory,
    ConfirmedDeletion,
}

impl SourceBackedZeroSourceAuthorityKind {
    #[doc(hidden)]
    pub const fn compact_code(self) -> char {
        match self {
            Self::CompleteEmptyInventory => 'e',
            Self::ConfirmedDeletion => 'd',
        }
    }

    #[doc(hidden)]
    pub fn from_compact_code(value: char) -> Result<Self> {
        match value {
            'e' => Ok(Self::CompleteEmptyInventory),
            'd' => Ok(Self::ConfirmedDeletion),
            _ => bail!("Core zero-source authority has an unknown disposition"),
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct SourceBackedZeroSourceAuthority {
    pub generation_id: String,
    pub route_identity: SourceRouteIdentity,
    pub kind: SourceBackedZeroSourceAuthorityKind,
}

impl SourceBackedZeroSourceAuthority {
    fn rebound_to(&self, generation_id: &str) -> Self {
        Self {
            generation_id: generation_id.to_owned(),
            route_identity: self.route_identity.clone(),
            kind: self.kind,
        }
    }
}

#[derive(Clone)]
pub struct SourceBackedRefreshPublication {
    pub generation_id: String,
    pub published_explicit_source_catalog: Option<ExplicitSourceCatalogAuthority>,
    pub unsupported_routes: usize,
    pub certified_source_count: usize,
    pub certified_source_bytes: u64,
    pub current: SourceBackedRefreshCurrent,
    pub timings: SourceBackedRefreshTimings,
    pub route_results: Vec<SourceBackedRefreshRouteResult>,
    pub zero_source_authority: Vec<SourceBackedZeroSourceAuthority>,
    pub catalog_route_bindings: Vec<ExplicitSourceCatalogRouteBinding>,
    pub verified_index: Option<Arc<VerifiedIndex>>,
}

impl fmt::Debug for SourceBackedRefreshPublication {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SourceBackedRefreshPublication")
            .field("generation_id", &self.generation_id)
            .field("route_results", &self.route_results)
            .field("has_verified_index", &self.verified_index.is_some())
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Default)]
pub struct SourceBackedRefreshCoveredPublication {
    pub route_results: Vec<SourceBackedRefreshRouteResult>,
    pub zero_source_authority: Vec<SourceBackedZeroSourceAuthority>,
    pub removed_source_count: usize,
    pub timings: SourceBackedRefreshTimings,
}

impl SourceBackedRefreshCoveredPublication {
    pub fn apply_receipt(&self, publication: &mut SourceBackedRefreshPublication) {
        publication
            .route_results
            .extend(self.route_results.iter().cloned());
        publication
            .route_results
            .sort_by(|left, right| left.route_identity.cmp(&right.route_identity));
        publication.zero_source_authority.extend(
            self.zero_source_authority
                .iter()
                .map(|authority| authority.rebound_to(&publication.generation_id)),
        );
        publication
            .zero_source_authority
            .sort_by(|left, right| left.route_identity.cmp(&right.route_identity));
        publication.current.removed_source_count = publication
            .current
            .removed_source_count
            .saturating_add(self.removed_source_count);
    }

    pub fn apply_timings(&self, publication: &mut SourceBackedRefreshPublication) {
        publication.timings.discovery_us = publication
            .timings
            .discovery_us
            .saturating_add(self.timings.discovery_us);
        publication.timings.scan_stage_us = publication
            .timings
            .scan_stage_us
            .saturating_add(self.timings.scan_stage_us);
        publication.timings.commit_us = publication
            .timings
            .commit_us
            .saturating_add(self.timings.commit_us);
    }

    pub fn apply(&self, publication: &mut SourceBackedRefreshPublication) {
        self.apply_receipt(publication);
        self.apply_timings(publication);
    }
}

pub struct PublishedSourceBackedState {
    pub verified_index: Option<VerifiedIndex>,
    pub explicit_source_catalog: Option<ExplicitSourceCatalogAuthority>,
    pub catalog_route_bindings: Vec<ExplicitSourceCatalogRouteBinding>,
    pub route_controls: BTreeMap<SourceRouteIdentity, Vec<u8>>,
}

pub trait PublishedSourceBackedStatePort: Send + Sync {
    fn open_published_state(&self, data_root: &Path) -> Result<PublishedSourceBackedState>;
}

#[doc(hidden)]
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct SourceBackedExactScanProgress {
    pub total_bytes: u64,
    pub completed_bytes: u64,
}

#[doc(hidden)]
pub struct SourceBackedRefreshProgressUpdate {
    pub phase: String,
    pub completed_sources: usize,
    pub total_sources: usize,
    pub total_sources_known: bool,
    pub current_source: Option<String>,
    pub completed_records: Option<u64>,
    pub completed_bytes: Option<u64>,
    pub providers: Vec<String>,
    pub processed_sessions: u64,
    pub processed_messages: u64,
    pub processed_tool_calls: u64,
    pub processed_bytes: u64,
    pub elapsed_millis: Option<u64>,
    pub current_source_progress: Option<SourceBackedCurrentSourceProgress>,
    pub exact_scan_progress: Option<SourceBackedExactScanProgress>,
}

#[derive(Clone)]
pub struct SourceBackedRefreshExecution<'a> {
    pub data_root: &'a Path,
    pub index_root: &'a Path,
    pub request_id: &'a str,
    pub operation: RefreshOperation,
    pub reconciliation_demand: SourceBackedReconciliationDemand,
    pub explicit_source_catalog: Option<&'a ExplicitSourceCatalogAuthority>,
    pub scope: SourceBackedRefreshScope,
    pub route_worksets: BTreeMap<SourceRouteIdentity, SourceBackedRefreshWorkset>,
    /// Immutable authenticated watch-catalog snapshot admitted with this
    /// request. It is absent for manual and uncertainty fallbacks.
    pub watch_catalog: Option<SourceBackedWatchCatalog>,
    pub covered_route_ids: BTreeSet<SourceRouteIdentity>,
    pub covered_publication: SourceBackedRefreshCoveredPublication,
    pub discovery_context: &'a DiscoveryContext,
    #[doc(hidden)]
    pub published_state: &'a dyn PublishedSourceBackedStatePort,
    report_progress: &'a dyn Fn(SourceBackedRefreshProgressUpdate) -> Result<()>,
}

impl<'a> SourceBackedRefreshExecution<'a> {
    #[doc(hidden)]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        data_root: &'a Path,
        index_root: &'a Path,
        request_id: &'a str,
        operation: RefreshOperation,
        explicit_source_catalog: Option<&'a ExplicitSourceCatalogAuthority>,
        scope: SourceBackedRefreshScope,
        covered_route_ids: BTreeSet<SourceRouteIdentity>,
        covered_publication: SourceBackedRefreshCoveredPublication,
        discovery_context: &'a DiscoveryContext,
        published_state: &'a dyn PublishedSourceBackedStatePort,
        report_progress: &'a dyn Fn(SourceBackedRefreshProgressUpdate) -> Result<()>,
    ) -> Self {
        let reconciliation_demand = match operation {
            RefreshOperation::Refresh => SourceBackedReconciliationDemand::Incremental,
            RefreshOperation::Import => SourceBackedReconciliationDemand::Exhaustive,
        };
        Self {
            data_root,
            index_root,
            request_id,
            operation,
            reconciliation_demand,
            explicit_source_catalog,
            scope,
            route_worksets: BTreeMap::new(),
            watch_catalog: None,
            covered_route_ids,
            covered_publication,
            discovery_context,
            published_state,
            report_progress,
        }
    }

    pub fn with_reconciliation_demand(mut self, demand: SourceBackedReconciliationDemand) -> Self {
        self.reconciliation_demand = demand;
        self
    }

    pub fn with_route_worksets(
        mut self,
        route_worksets: BTreeMap<SourceRouteIdentity, SourceBackedRefreshWorkset>,
    ) -> Self {
        self.route_worksets = route_worksets;
        self
    }

    pub fn with_watch_catalog_opt(mut self, catalog: Option<SourceBackedWatchCatalog>) -> Self {
        self.watch_catalog = catalog;
        self
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn report_detailed_progress_with_total_state(
        &self,
        phase: &str,
        completed_sources: usize,
        total_sources: usize,
        total_sources_known: bool,
        current_source: Option<String>,
        completed_records: Option<u64>,
        completed_bytes: Option<u64>,
        current_source_progress: Option<SourceBackedCurrentSourceProgress>,
    ) -> Result<()> {
        self.report_history_progress_with_total_state(
            phase,
            completed_sources,
            total_sources,
            total_sources_known,
            current_source,
            completed_records,
            completed_bytes,
            current_source_progress,
            Vec::new(),
            0,
            0,
            0,
            0,
            None,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn report_history_progress_with_total_state(
        &self,
        phase: &str,
        completed_sources: usize,
        total_sources: usize,
        total_sources_known: bool,
        current_source: Option<String>,
        completed_records: Option<u64>,
        completed_bytes: Option<u64>,
        current_source_progress: Option<SourceBackedCurrentSourceProgress>,
        providers: Vec<String>,
        processed_sessions: u64,
        processed_messages: u64,
        processed_tool_calls: u64,
        processed_bytes: u64,
        elapsed_millis: Option<u64>,
        exact_scan_progress: Option<SourceBackedExactScanProgress>,
    ) -> Result<()> {
        (self.report_progress)(SourceBackedRefreshProgressUpdate {
            phase: phase.to_owned(),
            completed_sources,
            total_sources,
            total_sources_known,
            current_source,
            completed_records,
            completed_bytes,
            providers,
            processed_sessions,
            processed_messages,
            processed_tool_calls,
            processed_bytes,
            elapsed_millis,
            current_source_progress,
            exact_scan_progress,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn report_detailed_progress(
        &self,
        phase: &str,
        completed_sources: usize,
        total_sources: usize,
        current_source: Option<String>,
        completed_records: Option<u64>,
        completed_bytes: Option<u64>,
        current_source_progress: Option<SourceBackedCurrentSourceProgress>,
    ) -> Result<()> {
        self.report_detailed_progress_with_total_state(
            phase,
            completed_sources,
            total_sources,
            true,
            current_source,
            completed_records,
            completed_bytes,
            current_source_progress,
        )
    }

    pub fn report_progress(
        &self,
        phase: &str,
        completed_sources: usize,
        total_sources: usize,
        current_source: Option<String>,
        completed_records: Option<u64>,
        completed_bytes: Option<u64>,
    ) -> Result<()> {
        self.report_detailed_progress(
            phase,
            completed_sources,
            total_sources,
            current_source,
            completed_records,
            completed_bytes,
            None,
        )
    }
}

pub fn nonzero_duration_micros(duration: StdDuration) -> u64 {
    u64::try_from(duration.as_micros())
        .unwrap_or(u64::MAX)
        .max(1)
}
