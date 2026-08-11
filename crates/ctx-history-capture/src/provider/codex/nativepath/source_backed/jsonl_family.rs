use std::{collections::BTreeMap, sync::Mutex};

#[cfg(any(test, ctx_codex_causal_qualification))]
use super::causal::CodexCausalLedgerV1;
use super::*;
use crate::{
    provider::source_backed::{
        family::jsonl::{
            observe_opened_file, observe_opened_file_allow_append, JsonlFamilyAdapter,
            JsonlFamilyAppendMode, JsonlFamilyBaseScope, JsonlFamilyExecutionIo,
            JsonlFamilyInventory, JsonlFamilyInventoryMode, JsonlFamilyLeaf,
            JsonlFamilyMembershipObservation, JsonlFamilyProjectionMode,
            JsonlFamilyRootMissingMode, JsonlFamilySemanticExecutor, JsonlFamilySemanticPage,
            JsonlFamilySemanticPreflight, JsonlFamilySemanticSummary, JsonlFamilyWorkerContext,
            JsonlFileObservation, JsonlRecordFraming,
        },
        SourceBackedRouteErrorKind,
    },
    Result,
};

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

fn carried_or_observe_generation_source_capability_v0(
    source: &CodexCatalogSource,
) -> Result<JsonlFileObservation> {
    match &source.carried_jsonl_observation {
        Some(observation) => Ok(observation.clone()),
        None => observe_generation_source_capability_v0(source),
    }
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
    scanner: Option<CodexNativeScanner>,
    #[cfg(any(test, ctx_codex_causal_qualification))]
    native_session_id: String,
    #[cfg(any(test, ctx_codex_causal_qualification))]
    projection_mode: JsonlFamilyProjectionMode,
}

impl CodexSessionSemanticExecutorV0 {
    fn new(
        state: Arc<Mutex<CodexSessionJsonlFamilyStateV0>>,
        leaf: &JsonlFamilyLeaf,
        _checkpoint: Option<&TypedKey>,
        base_event_lookup: Option<IndexBaseEventLookup>,
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
        // The shared family owns all append authority. Historical Codex
        // provider payloads are inert; the mandatory semantic preflight below
        // reconstructs owner and continuation state from the certified prefix.
        let base_event_lookup = match projection_mode {
            JsonlFamilyProjectionMode::CertifiedAppend => Some(base_event_lookup.ok_or(
                CaptureError::SystemInvariant("Codex semantic append has no base event lookup"),
            )?),
            JsonlFamilyProjectionMode::Cold | JsonlFamilyProjectionMode::Replacement => None,
        };
        let scanner = CodexNativeScanner::new_semantic(plan.0, base_event_lookup)?;
        Ok(Self {
            #[cfg(any(test, ctx_codex_causal_qualification))]
            state,
            scanner: Some(scanner),
            #[cfg(any(test, ctx_codex_causal_qualification))]
            native_session_id: plan.2,
            #[cfg(any(test, ctx_codex_causal_qualification))]
            projection_mode,
        })
    }
}

impl JsonlFamilySemanticExecutor for CodexSessionSemanticExecutorV0 {
    fn preflight(
        &mut self,
        input: &mut JsonlFamilyExecutionIo,
    ) -> Result<JsonlFamilySemanticPreflight> {
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
            .next_semantic_page(input, worker.repository_attributor())?
        else {
            return Ok(None);
        };
        Ok(Some(JsonlFamilySemanticPage::new(page.records)))
    }

    fn finish(mut self: Box<Self>) -> Result<JsonlFamilySemanticSummary> {
        let scan = self
            .scanner
            .take()
            .ok_or(CaptureError::SystemInvariant(
                "Codex semantic executor lost its scanner",
            ))?
            .finish_semantic()?;
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
                staged_documents: scan.counters.retained_records,
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
            None,
        ))
    }
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
    let state = state.lock().map_err(|_| codex_family_state_error())?;
    #[cfg(any(test, ctx_codex_causal_qualification))]
    let mut state = state;
    for leaf in leaves {
        if !state.plans.contains_key(leaf.source()) {
            return Err(CaptureError::InvalidPayload(
                "Codex JSONL family selected an unplanned leaf".to_owned(),
            ));
        }
    }
    #[cfg(any(test, ctx_codex_causal_qualification))]
    {
        let observations = state
            .plans
            .values()
            .map(|plan| {
                let exact_replay =
                    bases
                        .get(&plan.1.exact_descriptor_digest())
                        .is_some_and(|base| {
                            base.parser_revision() == adapter.parser_revision()
                                && base.observation().source().exact_descriptor_eq(&plan.1)
                        });
                (plan.2.clone(), exact_replay)
            })
            .collect::<Vec<_>>();
        for (native_session_id, exact_replay) in observations {
            state.causal.observe_catalog(
                &native_session_id,
                super::catalog::CodexCatalogWorkV0::default(),
                exact_replay,
            );
        }
        state.stage_pending = true;
    }
    #[cfg(not(any(test, ctx_codex_causal_qualification)))]
    let _ = (adapter, bases);
    Ok(None)
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
        let prepared = self.generation.prepared()?;
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
            let observation = carried_or_observe_generation_source_capability_v0(source)?;
            leaves.push(JsonlFamilyLeaf::bind_frozen_observed(
                source_key.clone(),
                source.source_path.clone(),
                Arc::clone(&authority),
                authority_path,
                TypedKey::utf8(native_session_id)
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
        let prepared = self.generation.prepared()?;
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
        causal.write_qualification_receipt()?;
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
        if let Some(roots) = self.generation.session_tree_roots() {
            self.discover_tree_family(root, roots)
        } else if let Some(input) = self.generation.explicit_session_input() {
            self.discover_explicit_family(root, input)
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
                    )?
                {
                    observation.bind_source_hint(path, codex_source_key(&native_session_id)?);
                }
            }
            Ok(observation)
        } else {
            JsonlFamilyMembershipObservation::observe(root, opening)
        }
    }

    fn discovery_error_kind(&self, error: &CaptureError) -> SourceBackedRouteErrorKind {
        match error {
            CaptureError::Io(error) if error.kind() == std::io::ErrorKind::NotFound => {
                SourceBackedRouteErrorKind::Unavailable
            }
            CaptureError::SystemIo { source, .. }
                if source.kind() == std::io::ErrorKind::NotFound =>
            {
                SourceBackedRouteErrorKind::ResourceUnavailable
            }
            _ => SourceBackedRouteErrorKind::InvalidSource,
        }
    }

    fn order_leaf_scans(&self, leaves: &mut [JsonlFamilyLeaf]) -> Result<()> {
        leaves.sort_by(|left, right| {
            left.source()
                .identity()
                .digest()
                .cmp(&right.source().identity().digest())
        });
        Ok(())
    }

    fn prepare_leaf_scans(
        &self,
        leaves: &[JsonlFamilyLeaf],
        bases: &HashMap<[u8; 32], &CertifiedSource>,
    ) -> Result<Option<usize>> {
        prepare_codex_session_jsonl_scans_v0(self, &self.state, leaves, bases)
    }

    fn semantic_executor(
        &self,
        leaf: &JsonlFamilyLeaf,
        checkpoint: Option<&TypedKey>,
        base_event_lookup: Option<IndexBaseEventLookup>,
        mode: JsonlFamilyProjectionMode,
    ) -> Result<Option<Box<dyn JsonlFamilySemanticExecutor>>> {
        Ok(Some(Box::new(CodexSessionSemanticExecutorV0::new(
            Arc::clone(&self.state),
            leaf,
            checkpoint,
            base_event_lookup,
            mode,
        )?)))
    }

    fn base_source_path(&self, _certificate: &CertifiedSource) -> Result<PathBuf> {
        if let Some(roots) = self.generation.session_tree_roots() {
            roots.first().cloned().ok_or(CaptureError::SystemInvariant(
                "Codex JSONL family has no route root",
            ))
        } else if let Some(input) = self.generation.explicit_session_input() {
            Ok(input.path().to_path_buf())
        } else {
            Err(CaptureError::SystemInvariant(
                "Codex generation route has no base source path",
            ))
        }
    }
}
