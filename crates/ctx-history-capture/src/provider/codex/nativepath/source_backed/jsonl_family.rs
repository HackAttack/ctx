use std::{collections::BTreeMap, sync::Mutex};

use chrono::{DateTime, Utc};

#[cfg(any(test, ctx_codex_causal_qualification))]
use super::causal::CodexCausalLedgerV1;
use super::*;
use crate::{
    common::io::OpenedProviderSourceFile,
    provider::source_backed::{
        family::jsonl::{
            observe_opened_file, observe_opened_file_allow_append, provider_checkpoint_for_base,
            JsonlFamilyAdapter, JsonlFamilyAppendMode, JsonlFamilyBaseScope,
            JsonlFamilyExecutionIo, JsonlFamilyInventory, JsonlFamilyInventoryMode,
            JsonlFamilyLeaf, JsonlFamilyMembershipObservation, JsonlFamilyProjectionMode,
            JsonlFamilyProjector, JsonlFamilyRootMissingMode, JsonlFamilySemanticExecutor,
            JsonlFamilySemanticPage, JsonlFamilySemanticPreflight, JsonlFamilySemanticSummary,
            JsonlFamilyWorkerContext, JsonlFileObservation, JsonlRecordFraming,
        },
        SourceBackedRouteErrorKind,
    },
    Result,
};

type CodexSessionPlanV0 = (CodexCatalogSource, SourceKey, String);

const LEGACY_CODEX_FRONTIER_KIND: &str = "codex-nativepath-checkpoint-v18";

fn observe_generation_source_capability_v0(
    source: &CodexCatalogSource,
) -> Result<JsonlFileObservation> {
    let opened = reopen_codex_source_capability(source)?;
    revalidate_codex_catalog_source_capability(source, &opened)?;
    let admitted = observe_opened_file(&source.source_path, &opened)?;
    let current = observe_opened_file_allow_append(&source.source_path, &opened)?;
    if !admitted.admits_frozen_prefix_in(&current) {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    Ok(admitted)
}

#[derive(Default)]
struct CodexSessionJsonlFamilyStateV0 {
    plans: HashMap<SourceKey, CodexSessionPlanV0>,
    #[cfg(any(test, ctx_codex_causal_qualification))]
    causal: CodexCausalLedgerV1,
    #[cfg(any(test, ctx_codex_causal_qualification))]
    stage_pending: bool,
}

struct CodexSessionSemanticExecutorV0 {
    #[cfg(any(test, ctx_codex_causal_qualification))]
    state: Arc<Mutex<CodexSessionJsonlFamilyStateV0>>,
    source_key: SourceKey,
    native_session_id: String,
    scanner: Option<CodexNativeScanner>,
    checkpoint: Option<super::super::checkpoint::CodexSemanticCheckpoint>,
    event_identity_state: CodexEventIdentityStateV0,
    projection_mode: JsonlFamilyProjectionMode,
    staged_documents: u64,
}

impl CodexSessionSemanticExecutorV0 {
    fn new(
        state: Arc<Mutex<CodexSessionJsonlFamilyStateV0>>,
        leaf: &JsonlFamilyLeaf,
        checkpoint: Option<&TypedKey>,
        base_event_lookup: Option<BaseEventIdentityLookup>,
        projection_mode: JsonlFamilyProjectionMode,
    ) -> Result<Self> {
        let plan = {
            let state = state.lock().map_err(|_| codex_family_state_error())?;
            state.plans.get(leaf.source()).cloned().ok_or_else(|| {
                CaptureError::InvalidPayload(
                    "Codex JSONL family leaf has no native source plan".to_owned(),
                )
            })?
        };
        if plan.0.source_path != leaf.source_path() {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let checkpoint = match (projection_mode, checkpoint) {
            (JsonlFamilyProjectionMode::CertifiedAppend, Some(checkpoint)) => {
                // Shared family validation has already certified the source and
                // physical frontier. Unknown provider-only state removes Codex
                // append proof; preflight below requests the one safe replacement.
                super::super::checkpoint::CodexSemanticCheckpoint::decode_key(checkpoint).ok()
            }
            (JsonlFamilyProjectionMode::CertifiedAppend, None) => None,
            (JsonlFamilyProjectionMode::Cold | JsonlFamilyProjectionMode::Replacement, _) => None,
        };
        let event_identity_state = match projection_mode {
            JsonlFamilyProjectionMode::CertifiedAppend => {
                CodexEventIdentityStateV0::for_append(base_event_lookup.ok_or(
                    CaptureError::SystemInvariant("Codex semantic append has no base event lookup"),
                )?)
            }
            JsonlFamilyProjectionMode::Cold | JsonlFamilyProjectionMode::Replacement => {
                CodexEventIdentityStateV0::default()
            }
        };
        let scanner = CodexNativeScanner::new_semantic(plan.0)?;
        Ok(Self {
            #[cfg(any(test, ctx_codex_causal_qualification))]
            state,
            source_key: plan.1,
            native_session_id: plan.2,
            scanner: Some(scanner),
            checkpoint,
            event_identity_state,
            projection_mode,
            staged_documents: 0,
        })
    }
}

impl JsonlFamilySemanticExecutor for CodexSessionSemanticExecutorV0 {
    fn preflight(
        &mut self,
        input: &mut JsonlFamilyExecutionIo,
    ) -> Result<JsonlFamilySemanticPreflight> {
        if self.projection_mode == JsonlFamilyProjectionMode::CertifiedAppend
            && self.checkpoint.is_none()
        {
            return Ok(JsonlFamilySemanticPreflight::RetryReplacement);
        }
        let retry = self
            .scanner
            .as_mut()
            .ok_or(CaptureError::SystemInvariant(
                "Codex semantic executor lost its scanner",
            ))?
            .preflight_semantic(input)?;
        Ok(if retry {
            JsonlFamilySemanticPreflight::RetryReplacement
        } else {
            JsonlFamilySemanticPreflight::Ready
        })
    }

    fn next_page(
        &mut self,
        input: &mut JsonlFamilyExecutionIo,
        worker: &mut JsonlFamilyWorkerContext,
    ) -> Result<Option<JsonlFamilySemanticPage>> {
        let Some(page) = self
            .scanner
            .as_mut()
            .ok_or(CaptureError::SystemInvariant(
                "Codex semantic executor lost its scanner",
            ))?
            .next_semantic_page(input)?
        else {
            return Ok(None);
        };
        let mut records = Vec::with_capacity(page.source_backed_rows.len());
        if !page.source_backed_rows.is_empty() {
            let owner = page.owner.as_ref().ok_or_else(|| {
                codex_family_capture_error(CodexSourceBackedErrorV0::MissingPageOwner)
            })?;
            validate_owner(owner, &self.native_session_id).map_err(codex_family_capture_error)?;
            let session_id = codex_session_identity(&self.source_key, &self.native_session_id)
                .map_err(codex_family_capture_error)?;
            for row in page.source_backed_rows {
                records.push(
                    codex_core_record(
                        &self.source_key,
                        session_id,
                        owner,
                        row,
                        &mut self.event_identity_state,
                        worker.repository_attributor(),
                    )
                    .map_err(codex_family_capture_error)?,
                );
                self.staged_documents = self.staged_documents.checked_add(1).ok_or_else(|| {
                    codex_family_capture_error(CodexSourceBackedErrorV0::CountOverflow)
                })?;
            }
        }
        Ok(Some(JsonlFamilySemanticPage::new(records)))
    }

    fn finish(mut self: Box<Self>) -> Result<JsonlFamilySemanticSummary> {
        let scan = self
            .scanner
            .take()
            .ok_or(CaptureError::SystemInvariant(
                "Codex semantic executor lost its scanner",
            ))?
            .finish_semantic()?;
        if scan.counters.retained_records != self.staged_documents {
            return Err(codex_family_capture_error(
                CodexSourceBackedErrorV0::ScanCountMismatch,
            ));
        }
        let provider_checkpoint = scan
            .checkpoint
            .map(|checkpoint| checkpoint.encode_key().map_err(CaptureError::from))
            .transpose()?;
        #[cfg(any(test, ctx_codex_causal_qualification))]
        {
            let mut counters = CodexSourceBackedCountersV0 {
                cold_sources: u64::from(self.projection_mode == JsonlFamilyProjectionMode::Cold),
                appended_sources: u64::from(
                    self.projection_mode == JsonlFamilyProjectionMode::CertifiedAppend,
                ),
                replaced_sources: u64::from(
                    self.projection_mode == JsonlFamilyProjectionMode::Replacement,
                ),
                writer_mutated_sources: 1,
                scanner_source_opens: 1,
                scanner_sources_started: 1,
                scanner_sources_completed: 1,
                staged_documents: self.staged_documents,
                ..CodexSourceBackedCountersV0::default()
            };
            counters.add_scan(scan.counters);
            let mut state = self.state.lock().map_err(|_| codex_family_state_error())?;
            state.causal.observe_scan(&self.native_session_id, counters);
            state.stage_pending = true;
        }
        Ok(JsonlFamilySemanticSummary::new(
            scan.counters.retained_records,
            scan.counters.rejected_complete_records,
            provider_checkpoint,
        ))
    }
}

fn codex_semantic_executor_v0(
    state: Arc<Mutex<CodexSessionJsonlFamilyStateV0>>,
    leaf: &JsonlFamilyLeaf,
    checkpoint: Option<&TypedKey>,
    base_event_lookup: Option<BaseEventIdentityLookup>,
    projection_mode: JsonlFamilyProjectionMode,
) -> Result<Box<dyn JsonlFamilySemanticExecutor>> {
    Ok(Box::new(CodexSessionSemanticExecutorV0::new(
        state,
        leaf,
        checkpoint,
        base_event_lookup,
        projection_mode,
    )?))
}

fn codex_family_state_error() -> CaptureError {
    CaptureError::InvalidPayload("Codex JSONL family state lock was poisoned".to_owned())
}

fn prepare_codex_session_jsonl_scans_v0(
    adapter: &dyn JsonlFamilyAdapter,
    state: &Mutex<CodexSessionJsonlFamilyStateV0>,
    leaves: &[JsonlFamilyLeaf],
    bases: &HashMap<[u8; 32], &CertifiedSource>,
) -> Result<Option<usize>> {
    let mut state = state.lock().map_err(|_| codex_family_state_error())?;
    if leaves
        .iter()
        .any(|leaf| !state.plans.contains_key(leaf.source()))
    {
        return Err(CaptureError::InvalidPayload(
            "Codex JSONL family selected an unplanned leaf".to_owned(),
        ));
    }
    let mut hydrated = HashMap::with_capacity(state.plans.len());
    for (_, plan) in std::mem::take(&mut state.plans) {
        let base = bases
            .get(&plan.1.exact_descriptor_digest())
            .copied()
            .filter(|base| base.observation().source().exact_descriptor_eq(&plan.1));
        let provider_checkpoint = match base {
            Some(base) if base.parser_revision() == adapter.parser_revision() => leaves
                .iter()
                .find(|leaf| leaf.source().exact_descriptor_eq(&plan.1))
                .map(|leaf| codex_provider_checkpoint_for_base(adapter, leaf, base))
                .transpose()?
                .flatten(),
            Some(_) | None => None,
        };
        let (plan, work, exact_replay) =
            super::catalog::hydrate_codex_session_plan_v0(plan, provider_checkpoint.as_ref())
                .map_err(codex_family_capture_error)?;
        #[cfg(any(test, ctx_codex_causal_qualification))]
        {
            state.causal.observe_catalog(
                &plan.2,
                plan.0.catalog_parent_native_session_id.as_deref(),
                work,
                exact_replay,
            );
        }
        #[cfg(not(any(test, ctx_codex_causal_qualification)))]
        let _ = (work, exact_replay);
        hydrated.insert(plan.1.clone(), plan);
    }
    state.plans = hydrated;
    #[cfg(any(test, ctx_codex_causal_qualification))]
    {
        state.stage_pending = true;
    }
    Ok(None)
}

fn codex_provider_checkpoint_for_base(
    adapter: &dyn JsonlFamilyAdapter,
    leaf: &JsonlFamilyLeaf,
    base: &CertifiedSource,
) -> Result<Option<TypedKey>> {
    base.validate_contract()
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    if base
        .frontier()
        .is_some_and(|frontier| frontier.checkpoint_kind() == LEGACY_CODEX_FRONTIER_KIND)
    {
        // V18 is the released Codex-native envelope that immediately preceded
        // the shared family checkpoint. Its physical and semantic state cannot
        // be resumed independently, so migrate it only by a full replacement.
        return Ok(None);
    }
    provider_checkpoint_for_base(adapter, leaf, base)
}

fn order_codex_session_jsonl_scans_v0(
    _state: &Mutex<CodexSessionJsonlFamilyStateV0>,
    leaves: &mut [JsonlFamilyLeaf],
) -> Result<()> {
    leaves.sort_by(|left, right| {
        left.source()
            .identity()
            .digest()
            .cmp(&right.source().identity().digest())
    });
    Ok(())
}

fn install_prepared_state_v0(
    state: &Mutex<CodexSessionJsonlFamilyStateV0>,
    plans: &[CodexSessionPlanV0],
    _completed_stage: bool,
) -> Result<()> {
    let mut state = state.lock().map_err(|_| codex_family_state_error())?;
    state.plans = plans
        .iter()
        .cloned()
        .map(|plan| (plan.1.clone(), plan))
        .collect();
    #[cfg(any(test, ctx_codex_causal_qualification))]
    {
        state.causal = CodexCausalLedgerV1::default();
    }
    #[cfg(test)]
    if plans.is_empty() && !_completed_stage {
        state.stage_pending = true;
    }
    Ok(())
}

fn missing_codex_inventory_v0(
    state: &Mutex<CodexSessionJsonlFamilyStateV0>,
    route_path: &Path,
    _completed_stage: bool,
) -> Result<JsonlFamilyInventory> {
    let mut state = state.lock().map_err(|_| codex_family_state_error())?;
    state.plans.clear();
    #[cfg(any(test, ctx_codex_causal_qualification))]
    {
        state.causal = CodexCausalLedgerV1::default();
    }
    #[cfg(test)]
    if !_completed_stage {
        state.stage_pending = true;
    }
    JsonlFamilyInventory::missing(CaptureProvider::Codex, route_path)
}

/// Codex session inventory and semantic JSONL execution for either explicit or
/// tree discovery. Shared JSONL owns carried bases and the physical lifecycle;
/// its mandatory generation route owns discovery and terminal membership.
#[derive(Clone)]
pub(crate) struct CodexSessionJsonlFamilyAdapterV0 {
    state: Arc<Mutex<CodexSessionJsonlFamilyStateV0>>,
    generation: CodexGenerationRouteV0,
}

impl CodexSessionJsonlFamilyAdapterV0 {
    pub(crate) fn new(generation: CodexGenerationRouteV0) -> Self {
        Self {
            state: Arc::new(Mutex::new(CodexSessionJsonlFamilyStateV0::default())),
            generation,
        }
    }

    fn discover_tree_family(
        &self,
        route_root: &Path,
        roots: &[PathBuf],
    ) -> Result<JsonlFamilyInventory> {
        // Shared JSONL invokes this only after route admission and freezes the
        // opening inventory before leaf workers start.
        let prepared = self
            .generation
            .prepared()
            .map_err(codex_family_capture_error)?;
        if prepared.missing {
            return Err(CaptureError::SystemInvariant(
                "Codex session-tree generation partition is missing",
            ));
        }
        let plans = prepared.sources;
        let mut ordered_sources = (0..plans.len()).collect::<Vec<_>>();
        ordered_sources.sort_by_key(|index| plans[*index].1.identity().digest());
        let mut authorities = BTreeMap::<PathBuf, Arc<ProviderSourceRoot>>::new();
        let mut leaves = Vec::with_capacity(plans.len());
        for index in ordered_sources {
            let (source, source_key, native_session_id) = plans.get(index).ok_or(
                CaptureError::SystemInvariant("Codex generation source ordering changed"),
            )?;
            let authority = Arc::new(source.authority_root.clone().ok_or(
                CaptureError::SystemInvariant("Codex catalog source has no retained root"),
            )?);
            let authority_path =
                source
                    .authority_relative_path
                    .clone()
                    .ok_or(CaptureError::SystemInvariant(
                        "Codex catalog source has no authority path",
                    ))?;
            let observation = observe_generation_source_capability_v0(source)?;
            leaves.push(JsonlFamilyLeaf::bind_frozen_observed(
                source_key.clone(),
                source.source_path.clone(),
                Arc::clone(&authority),
                authority_path,
                TypedKey::utf8(&*native_session_id)
                    .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?,
                observation,
            ));
            authorities
                .entry(authority.named_path().to_path_buf())
                .or_insert(authority);
        }
        for root in roots {
            if !authorities.contains_key(root) {
                let authority = Arc::new(ProviderSourceRoot::open(root)?);
                authorities.insert(authority.named_path().to_path_buf(), authority);
            }
        }
        let inventory = JsonlFamilyInventory::present_multi(
            CaptureProvider::Codex,
            route_root,
            authorities.into_values().collect(),
            leaves,
        )?;
        install_prepared_state_v0(&self.state, &plans, false)?;
        Ok(inventory)
    }

    fn discover_explicit_family(
        &self,
        route_path: &Path,
        input: &CodexExplicitSessionSourceBackedInputV0,
    ) -> Result<JsonlFamilyInventory> {
        let completed_stage = self.run_pending_stage_observer()?;
        if route_path != input.path() {
            return Err(CaptureError::InvalidPayload(
                "explicit Codex JSONL route path changed".to_owned(),
            ));
        }
        let prepared = self
            .generation
            .prepared()
            .map_err(codex_family_capture_error)?;
        if prepared.missing {
            return missing_codex_inventory_v0(&self.state, route_path, completed_stage);
        }
        let parent = route_path.parent().ok_or_else(|| {
            CaptureError::InvalidPayload("explicit Codex JSONL path has no parent".to_owned())
        })?;
        let authority_path = route_path.file_name().map(PathBuf::from).ok_or_else(|| {
            CaptureError::InvalidPayload("explicit Codex JSONL path has no filename".to_owned())
        })?;
        let authority = Arc::new(ProviderSourceRoot::open(parent)?);
        let plans = prepared.sources;
        let mut leaves = Vec::with_capacity(plans.len());
        for plan in &plans {
            let observation = observe_generation_source_capability_v0(&plan.0)?;
            leaves.push(JsonlFamilyLeaf::bind_frozen_observed(
                plan.1.clone(),
                plan.0.source_path.clone(),
                Arc::clone(&authority),
                authority_path.clone(),
                TypedKey::utf8(&plan.2)
                    .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?,
                observation,
            ));
        }
        let inventory =
            JsonlFamilyInventory::present(CaptureProvider::Codex, route_path, authority, leaves)?;
        install_prepared_state_v0(&self.state, &plans, completed_stage)?;
        Ok(inventory)
    }

    #[cfg(any(test, ctx_codex_causal_qualification))]
    fn run_pending_stage_observer(&self) -> Result<bool> {
        let causal = {
            let mut state = self.state.lock().map_err(|_| codex_family_state_error())?;
            if !state.stage_pending {
                return Ok(false);
            }
            state.stage_pending = false;
            std::mem::take(&mut state.causal)
        };
        #[cfg(test)]
        causal.run_test_observer();
        causal
            .write_qualification_receipt()
            .map_err(codex_family_capture_error)?;
        Ok(true)
    }

    #[cfg(not(any(test, ctx_codex_causal_qualification)))]
    fn run_pending_stage_observer(&self) -> Result<bool> {
        Ok(false)
    }
}

impl JsonlFamilyAdapter for CodexSessionJsonlFamilyAdapterV0 {
    fn provider(&self) -> CaptureProvider {
        CaptureProvider::Codex
    }

    fn source_format(&self) -> &'static str {
        CODEX_SESSION_SOURCE_FORMAT
    }

    fn schema_variant(&self) -> &'static str {
        CODEX_SOURCE_SCHEMA_VARIANT
    }

    fn parser_revision(&self) -> &'static str {
        CODEX_PARSER_REVISION
    }

    fn append_mode(&self) -> JsonlFamilyAppendMode {
        JsonlFamilyAppendMode::CertifiedSuffix
    }

    fn record_framing(&self) -> JsonlRecordFraming {
        JsonlRecordFraming::terminal_nul_padded(crate::MAX_PROVIDER_JSONL_LINE_BYTES)
    }

    fn bind_admitted_eof(&self) -> bool {
        true
    }

    fn root_missing_mode(&self) -> JsonlFamilyRootMissingMode {
        if self.generation.is_session_tree() {
            JsonlFamilyRootMissingMode::Unavailable
        } else {
            JsonlFamilyRootMissingMode::AuthoritativeEmpty
        }
    }

    fn inventory_mode(&self) -> JsonlFamilyInventoryMode {
        JsonlFamilyInventoryMode::FrozenOpeningAllowAdditions
    }

    fn base_scope(&self) -> JsonlFamilyBaseScope {
        JsonlFamilyBaseScope::Route
    }

    fn discover(&self, root: &Path) -> Result<JsonlFamilyInventory> {
        if let Some(roots) = self
            .generation
            .session_tree_roots()
            .map_err(codex_family_capture_error)?
        {
            self.discover_tree_family(root, &roots)
        } else if let Some(input) = self
            .generation
            .explicit_session_input()
            .map_err(codex_family_capture_error)?
        {
            self.discover_explicit_family(root, &input)
        } else {
            Err(CaptureError::SystemInvariant(
                "Codex generation route has no discovery authority",
            ))
        }
    }

    fn observe_terminal_membership(
        &self,
        root: &Path,
        opening: &JsonlFamilyInventory,
    ) -> Result<JsonlFamilyMembershipObservation> {
        self.run_pending_stage_observer()?;
        if self.generation.is_session_tree() {
            let mut observation = JsonlFamilyMembershipObservation::observe_authorities(opening)?;
            let candidates = observation
                .unbound_routes()
                .map(|(path, authority, authority_path)| {
                    (path.to_path_buf(), authority, authority_path.to_path_buf())
                })
                .collect::<Vec<_>>();
            for (path, authority, authority_path) in candidates {
                if let Some(native_session_id) =
                    super::catalog::codex_terminal_native_session_id_hint(
                        &path,
                        &authority,
                        &authority_path,
                    )
                    .map_err(codex_family_capture_error)?
                {
                    observation.bind_source_hint(
                        path,
                        codex_source_key(&native_session_id).map_err(codex_family_capture_error)?,
                    );
                }
            }
            Ok(observation)
        } else {
            JsonlFamilyMembershipObservation::observe(root, opening)
        }
    }

    fn discovery_error_kind(&self, error: &CaptureError) -> SourceBackedRouteErrorKind {
        codex_discovery_error_kind(error)
    }

    fn scan_error_kind(&self, error: &CaptureError) -> SourceBackedRouteErrorKind {
        codex_scan_error_kind(error)
    }

    fn order_leaf_scans(&self, leaves: &mut [JsonlFamilyLeaf]) -> Result<()> {
        order_codex_session_jsonl_scans_v0(&self.state, leaves)
    }

    fn prepare_leaf_scans(
        &self,
        leaves: &[JsonlFamilyLeaf],
        bases: &HashMap<[u8; 32], &CertifiedSource>,
    ) -> Result<Option<usize>> {
        prepare_codex_session_jsonl_scans_v0(self, &self.state, leaves, bases)
    }

    fn projector(
        &self,
        _leaf: &JsonlFamilyLeaf,
        _source_file: Arc<OpenedProviderSourceFile>,
        _imported_at: DateTime<Utc>,
    ) -> Result<Box<dyn JsonlFamilyProjector>> {
        Err(CaptureError::SystemInvariant(
            "Codex JSONL leaves require the native semantic executor",
        ))
    }

    fn semantic_executor(
        &self,
        leaf: &JsonlFamilyLeaf,
        checkpoint: Option<&TypedKey>,
        base_event_lookup: Option<BaseEventIdentityLookup>,
        mode: JsonlFamilyProjectionMode,
    ) -> Result<Option<Box<dyn JsonlFamilySemanticExecutor>>> {
        codex_semantic_executor_v0(
            Arc::clone(&self.state),
            leaf,
            checkpoint,
            base_event_lookup,
            mode,
        )
        .map(Some)
    }

    fn base_source_path(&self, _certificate: &CertifiedSource) -> Result<PathBuf> {
        if let Some(roots) = self
            .generation
            .session_tree_roots()
            .map_err(codex_family_capture_error)?
        {
            roots.first().cloned().ok_or(CaptureError::SystemInvariant(
                "Codex JSONL family has no route root",
            ))
        } else if let Some(input) = self
            .generation
            .explicit_session_input()
            .map_err(codex_family_capture_error)?
        {
            Ok(input.path().to_path_buf())
        } else {
            Err(CaptureError::SystemInvariant(
                "Codex generation route has no base source path",
            ))
        }
    }
}

fn codex_family_capture_error(error: CodexSourceBackedErrorV0) -> CaptureError {
    match error {
        CodexSourceBackedErrorV0::Capture(error) => error,
        CodexSourceBackedErrorV0::Io(error) => CaptureError::Io(error),
        CodexSourceBackedErrorV0::Json(error) => CaptureError::Json(error),
        error => CaptureError::InvalidPayload(error.to_string()),
    }
}

fn codex_discovery_error_kind(error: &CaptureError) -> SourceBackedRouteErrorKind {
    if let Some(kind) = codex_systemic_error_kind(error) {
        return kind;
    }
    match error {
        CaptureError::SourceChangedDuringCapture => SourceBackedRouteErrorKind::SourceChanged,
        CaptureError::Io(error) if error.kind() == std::io::ErrorKind::NotFound => {
            SourceBackedRouteErrorKind::Unavailable
        }
        _ => SourceBackedRouteErrorKind::InvalidSource,
    }
}

fn codex_scan_error_kind(error: &CaptureError) -> SourceBackedRouteErrorKind {
    if let Some(kind) = codex_systemic_error_kind(error) {
        return kind;
    }
    match error {
        CaptureError::SourceChangedDuringCapture => SourceBackedRouteErrorKind::SourceChanged,
        CaptureError::Io(error) if error.kind() == std::io::ErrorKind::NotFound => {
            SourceBackedRouteErrorKind::SourceChanged
        }
        _ => SourceBackedRouteErrorKind::InvalidSource,
    }
}

fn codex_systemic_error_kind(error: &CaptureError) -> Option<SourceBackedRouteErrorKind> {
    match error {
        CaptureError::Io(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        CaptureError::Io(_) | CaptureError::SystemIo { .. } => {
            Some(SourceBackedRouteErrorKind::ResourceUnavailable)
        }
        CaptureError::SystemInvariant(_) | CaptureError::WorkerPanicked(_) => {
            Some(SourceBackedRouteErrorKind::Internal)
        }
        _ => None,
    }
}
