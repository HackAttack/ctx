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
    pub(super) rejections: Vec<CodexRootConflictRejectedSourceV0>,
    pub(super) prehydrated: bool,
    #[cfg(any(test, ctx_codex_causal_qualification))]
    pub(super) catalog_observations: Vec<CodexPreparedCatalogObservationV0>,
    #[cfg(any(test, ctx_codex_causal_qualification))]
    pub(super) work: CodexCatalogWorkV0,
}

pub(crate) struct CodexGenerationCarriedRouteV0 {
    pub(crate) participant: usize,
    pub(crate) sources: HashMap<SourceKey, CertifiedSource>,
}

#[cfg(any(test, ctx_codex_causal_qualification))]
#[derive(Clone)]
pub(super) struct CodexPreparedCatalogObservationV0 {
    pub(super) native_session_id: String,
    pub(super) parent_native_session_id: Option<String>,
    pub(super) work: CodexCatalogWorkV0,
    pub(super) exact_replay: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub(super) struct CodexRootConflictV0 {
    computed_root_native_session_id: String,
    conflicting_advisory_session_id: String,
    evidence_source_record: String,
    computed_root_source_record: String,
    advisory_source_record: Option<String>,
}

impl CodexRootConflictV0 {
    pub(super) fn diagnostic(&self) -> String {
        format!(
            "codex_lineage_root_conflict_v0 computed_root_native_session_id={} \
             conflicting_advisory_session_id={} evidence_source_record={} \
             computed_root_source_record={} advisory_source_record={}",
            self.computed_root_native_session_id,
            self.conflicting_advisory_session_id,
            self.evidence_source_record,
            self.computed_root_source_record,
            self.advisory_source_record
                .as_deref()
                .unwrap_or("unavailable"),
        )
    }
}

#[derive(Clone)]
pub(super) struct CodexRootConflictRejectedSourceV0 {
    pub(super) plan: CodexSessionPlanV0,
    pub(super) conflict: CodexRootConflictV0,
}

fn quarantine_root_conflicts_v0(
    sources: Vec<CodexSessionPlanV0>,
) -> (
    Vec<CodexSessionPlanV0>,
    Vec<CodexRootConflictRejectedSourceV0>,
) {
    let indices = sources
        .iter()
        .enumerate()
        .map(|(index, plan)| (plan.2.as_str(), index))
        .collect::<HashMap<_, _>>();
    let parents = sources
        .iter()
        .map(|plan| {
            plan.0
                .catalog_parent_native_session_id
                .as_deref()
                .and_then(|parent| indices.get(parent).copied())
        })
        .collect::<Vec<_>>();
    let mut roots = vec![None; sources.len()];
    for (start, root) in roots.iter_mut().enumerate() {
        let mut current = start;
        for _ in 0..=sources.len() {
            match parents[current] {
                Some(parent) => current = parent,
                None => {
                    *root = Some(current);
                    break;
                }
            }
        }
    }
    let mut evidence_order = (0..sources.len()).collect::<Vec<_>>();
    evidence_order.sort_by(|left, right| sources[*left].2.cmp(&sources[*right].2));
    let mut conflicts = HashMap::<usize, CodexRootConflictV0>::new();
    for index in evidence_order {
        let Some(advisory) = sources[index].0.catalog_advisory_session_id.as_deref() else {
            continue;
        };
        let Some(parent) = parents[index] else {
            // A child whose direct parent is outside this route remains an
            // unresolved child-local claim; another route must not become a
            // prerequisite for admitting its own bytes.
            continue;
        };
        let mut corroborated = advisory == sources[index].2;
        let mut ancestor = Some(parent);
        for _ in 0..sources.len() {
            let Some(candidate) = ancestor else { break };
            corroborated |= advisory == sources[candidate].2;
            ancestor = parents[candidate];
        }
        let Some(root) = roots[index] else { continue };
        if corroborated {
            continue;
        }
        conflicts
            .entry(root)
            .or_insert_with(|| CodexRootConflictV0 {
                computed_root_native_session_id: sources[root].2.clone(),
                conflicting_advisory_session_id: advisory.to_owned(),
                evidence_source_record: format!("session_meta:{}", sources[index].2),
                computed_root_source_record: format!("session_meta:{}", sources[root].2),
                advisory_source_record: indices
                    .contains_key(advisory)
                    .then(|| format!("session_meta:{advisory}")),
            });
    }
    let mut accepted = Vec::with_capacity(sources.len());
    let mut rejected = Vec::new();
    for (index, plan) in sources.into_iter().enumerate() {
        match roots[index].and_then(|root| conflicts.get(&root)).cloned() {
            Some(conflict) => rejected.push(CodexRootConflictRejectedSourceV0 { plan, conflict }),
            None => accepted.push(plan),
        }
    }
    (accepted, rejected)
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

    pub(crate) fn prepare(
        &self,
        selected: &[usize],
        carried: Vec<CodexGenerationCarriedRouteV0>,
    ) -> CodexSourceBackedResultV0<()> {
        // Participant IDs encode registration order. Preserve that order when
        // overlapping automatic and explicit routes establish exact source
        // ownership, independent of HashMap randomization.
        let selected = selected.iter().copied().collect::<BTreeSet<_>>();
        let carried = carried
            .into_iter()
            .map(|route| (route.participant, route.sources))
            .collect::<HashMap<_, _>>();
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
            let mut catalog_observations = Vec::with_capacity(discovered.len());
            for plan in discovered {
                let base = carried
                    .get(&participant)
                    .and_then(|sources| sources.get(&plan.1));
                let (plan, hydration_work, exact_replay) =
                    super::catalog::hydrate_codex_session_plan_v0(plan, base)?;
                #[cfg(any(test, ctx_codex_causal_qualification))]
                catalog_observations.push(CodexPreparedCatalogObservationV0 {
                    native_session_id: plan.2.clone(),
                    parent_native_session_id: plan.0.catalog_parent_native_session_id.clone(),
                    work: hydration_work,
                    exact_replay,
                });
                #[cfg(not(any(test, ctx_codex_causal_qualification)))]
                let _ = (hydration_work, exact_replay);
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
            let (sources, rejections) = quarantine_root_conflicts_v0(sources);
            routes.insert(
                participant,
                CodexPreparedRouteV0 {
                    missing,
                    sources,
                    rejections,
                    prehydrated: true,
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
