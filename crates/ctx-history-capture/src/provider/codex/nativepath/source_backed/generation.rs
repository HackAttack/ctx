use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    path::PathBuf,
    sync::{Arc, Mutex},
};

use super::*;

type CodexSessionPlanV0 = (CodexCatalogSource, SourceKey, String);

#[derive(Debug, Clone)]
enum CodexGenerationParticipantV0 {
    SessionTree {
        roots: Arc<[PathBuf]>,
    },
    ExplicitSession {
        input: CodexExplicitSessionSourceBackedInputV0,
    },
}

#[derive(Clone)]
pub(crate) struct CodexGenerationRouteV0 {
    coordinator: Arc<CodexGenerationNormalizationCoordinatorV0>,
    participant: usize,
}

impl CodexGenerationRouteV0 {
    pub(crate) fn participant(&self) -> usize {
        self.participant
    }

    pub(super) fn prepared(&self) -> CodexSourceBackedResultV0<CodexPreparedRouteV0> {
        self.coordinator.prepared(self.participant)
    }
}

#[derive(Clone)]
pub(super) struct CodexPreparedRouteV0 {
    pub(super) missing: bool,
    pub(super) sources: Vec<CodexSessionPlanV0>,
    pub(super) prehydrated: bool,
    #[cfg(any(test, ctx_codex_causal_qualification))]
    pub(super) catalog_observations: Vec<CodexPreparedCatalogObservationV0>,
    #[cfg(any(test, ctx_codex_causal_qualification))]
    pub(super) work: CodexCatalogWorkV0,
}

#[cfg(any(test, ctx_codex_causal_qualification))]
#[derive(Clone)]
pub(super) struct CodexPreparedCatalogObservationV0 {
    pub(super) native_session_id: String,
    pub(super) parent_native_session_id: Option<String>,
    pub(super) work: CodexCatalogWorkV0,
    pub(super) exact_replay: bool,
}

struct CodexPreparedGenerationV0 {
    routes: HashMap<usize, CodexPreparedRouteV0>,
}

#[derive(Default)]
struct CodexGenerationCoordinatorStateV0 {
    next_participant: usize,
    participants: BTreeMap<usize, CodexGenerationParticipantV0>,
    prepared: Option<CodexPreparedGenerationV0>,
}

/// Coordinates route registration without coupling one Codex source to another.
///
/// The shared publication layer still selects routes and supplies their carried
/// certificates. This provider-owned coordinator performs only route-local
/// inventory. It never opens, parses, scans, revalidates, or schedules an
/// ancestor or descendant on behalf of a selected leaf.
#[derive(Default)]
pub(crate) struct CodexGenerationNormalizationCoordinatorV0 {
    state: Mutex<CodexGenerationCoordinatorStateV0>,
}

impl std::fmt::Debug for CodexGenerationNormalizationCoordinatorV0 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("CodexGenerationNormalizationCoordinatorV0")
    }
}

impl CodexGenerationNormalizationCoordinatorV0 {
    pub(crate) fn register_session_tree(
        self: &Arc<Self>,
        roots: Vec<PathBuf>,
    ) -> CodexSourceBackedResultV0<CodexGenerationRouteV0> {
        self.register(CodexGenerationParticipantV0::SessionTree {
            roots: roots.into(),
        })
    }

    pub(crate) fn register_explicit_session(
        self: &Arc<Self>,
        input: CodexExplicitSessionSourceBackedInputV0,
    ) -> CodexSourceBackedResultV0<CodexGenerationRouteV0> {
        self.register(CodexGenerationParticipantV0::ExplicitSession { input })
    }

    fn register(
        self: &Arc<Self>,
        participant: CodexGenerationParticipantV0,
    ) -> CodexSourceBackedResultV0<CodexGenerationRouteV0> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| CodexSourceBackedErrorV0::GenerationCoordinatorUnavailable)?;
        let id = state.next_participant;
        state.next_participant = state
            .next_participant
            .checked_add(1)
            .ok_or(CodexSourceBackedErrorV0::GenerationParticipantCountOverflow)?;
        state.participants.insert(id, participant);
        state.prepared = None;
        Ok(CodexGenerationRouteV0 {
            coordinator: Arc::clone(self),
            participant: id,
        })
    }

    pub(crate) fn prepare(&self, selected: &[usize]) -> CodexSourceBackedResultV0<()> {
        // Participant IDs encode registration order. Preserve that order when
        // overlapping automatic and explicit routes establish exact source
        // ownership, independent of HashMap randomization.
        let selected = selected.iter().copied().collect::<BTreeSet<_>>();
        let participants = {
            let state = self
                .state
                .lock()
                .map_err(|_| CodexSourceBackedErrorV0::GenerationCoordinatorUnavailable)?;
            selected
                .iter()
                .map(|id| {
                    state
                        .participants
                        .get(id)
                        .cloned()
                        .map(|participant| (*id, participant))
                        .ok_or(CodexSourceBackedErrorV0::GenerationCoordinatorUnavailable)
                })
                .collect::<CodexSourceBackedResultV0<Vec<_>>>()?
        };

        let mut routes = HashMap::with_capacity(participants.len());
        let mut established_owners = HashMap::<(PathBuf, String), usize>::new();
        let mut descriptor_bindings = HashMap::<[u8; 32], (SourceKey, String)>::new();
        for (participant, discovery) in participants {
            let (missing, discovered, work) = match discovery {
                CodexGenerationParticipantV0::SessionTree { roots } => {
                    let inventory =
                        super::catalog::discover_codex_deferred_session_tree_inventory_v0(&roots)?;
                    #[cfg(any(test, ctx_codex_causal_qualification))]
                    let work = inventory.work;
                    #[cfg(not(any(test, ctx_codex_causal_qualification)))]
                    let work = ();
                    (false, inventory.sources, work)
                }
                CodexGenerationParticipantV0::ExplicitSession { input } => {
                    let inventory = observe_codex_explicit_session_source_backed_v0(&input)?;
                    let plan = inventory.source_plan();
                    #[cfg(any(test, ctx_codex_causal_qualification))]
                    let work = CodexCatalogWorkV0::default();
                    #[cfg(not(any(test, ctx_codex_causal_qualification)))]
                    let work = ();
                    (plan.is_none(), plan.into_iter().collect(), work)
                }
            };
            #[cfg(not(any(test, ctx_codex_causal_qualification)))]
            let _ = work;

            let mut sources = Vec::with_capacity(discovered.len());
            #[cfg(any(test, ctx_codex_causal_qualification))]
            let catalog_observations = Vec::new();
            for plan in discovered {
                let descriptor = plan.1.exact_descriptor_digest();
                if let Some((existing, native_session_id)) = descriptor_bindings.get(&descriptor) {
                    if !existing.exact_descriptor_eq(&plan.1) || native_session_id != &plan.2 {
                        return Err(CaptureError::SystemInvariant(
                            "Codex generation source descriptor digest collision",
                        )
                        .into());
                    }
                } else {
                    descriptor_bindings.insert(descriptor, (plan.1.clone(), plan.2.clone()));
                }

                let observation = (plan.0.source_path.clone(), plan.2.clone());
                if established_owners
                    .insert(observation, participant)
                    .is_some()
                {
                    continue;
                }

                // No route may reconstruct ancestry from another source. A
                // changed leaf derives its local root from its own direct
                // parent field during hydration; an exact leaf restores that
                // same child-local tuple from its own checkpoint.
                sources.push(plan);
            }
            routes.insert(
                participant,
                CodexPreparedRouteV0 {
                    missing,
                    sources,
                    // Family checkpoint unwrapping requires the admitted leaf.
                    // Defer both exact replay and cold metadata hydration to
                    // `prepare_leaf_scans`, before ordering or publication.
                    prehydrated: false,
                    #[cfg(any(test, ctx_codex_causal_qualification))]
                    catalog_observations,
                    #[cfg(any(test, ctx_codex_causal_qualification))]
                    work,
                },
            );
        }

        self.state
            .lock()
            .map_err(|_| CodexSourceBackedErrorV0::GenerationCoordinatorUnavailable)?
            .prepared = Some(CodexPreparedGenerationV0 { routes });
        Ok(())
    }

    fn prepared(&self, participant: usize) -> CodexSourceBackedResultV0<CodexPreparedRouteV0> {
        self.state
            .lock()
            .map_err(|_| CodexSourceBackedErrorV0::GenerationCoordinatorUnavailable)?
            .prepared
            .as_ref()
            .and_then(|prepared| prepared.routes.get(&participant))
            .cloned()
            .ok_or(CodexSourceBackedErrorV0::GenerationCoordinatorUnavailable)
    }
}
