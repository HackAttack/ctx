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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct CodexRootConflictWorkV0 {
    source_claims_examined: u64,
    advisory_claims_examined: u64,
    identity_comparisons: u64,
}

fn advisory_contradicts_child_local_claim_v0(
    native_session_id: &str,
    child_local_root_native_session_id: &str,
    advisory_session_id: &str,
    work: &mut CodexRootConflictWorkV0,
) -> bool {
    work.advisory_claims_examined += 1;
    work.identity_comparisons += 1;
    if advisory_session_id == native_session_id {
        return false;
    }
    work.identity_comparisons += 1;
    advisory_session_id != child_local_root_native_session_id
}

fn quarantine_root_conflicts_v0(
    sources: Vec<CodexSessionPlanV0>,
) -> (
    Vec<CodexSessionPlanV0>,
    Vec<CodexRootConflictRejectedSourceV0>,
    CodexRootConflictWorkV0,
) {
    let mut work = CodexRootConflictWorkV0::default();
    let mut accepted = Vec::with_capacity(sources.len());
    let mut rejected = Vec::new();
    for plan in sources {
        work.source_claims_examined += 1;
        let conflict = plan
            .0
            .catalog_advisory_session_id
            .as_deref()
            .zip(plan.0.catalog_root_native_session_id.as_deref())
            .filter(|(advisory, child_local_root)| {
                advisory_contradicts_child_local_claim_v0(
                    &plan.2,
                    child_local_root,
                    advisory,
                    &mut work,
                )
            })
            .map(|(advisory, child_local_root)| CodexRootConflictV0 {
                computed_root_native_session_id: child_local_root.to_owned(),
                conflicting_advisory_session_id: advisory.to_owned(),
                evidence_source_record: format!("session_meta:{}", plan.2),
                computed_root_source_record: format!("session_meta:{child_local_root}"),
                // Target presence must never change this source's validity.
                advisory_source_record: None,
            });
        if let Some(conflict) = conflict {
            rejected.push(CodexRootConflictRejectedSourceV0 { plan, conflict });
        } else {
            accepted.push(plan);
        }
    }
    (accepted, rejected, work)
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
            let (sources, rejections, _root_conflict_work) = quarantine_root_conflicts_v0(sources);
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

#[cfg(test)]
mod root_conflict_tests {
    use super::*;

    #[test]
    fn source_local_root_conflict_validation_is_linear_at_catalog_limit_for_deep_chain() {
        const SOURCES: u64 = 131_072;
        let mut work = CodexRootConflictWorkV0::default();
        for index in 0..SOURCES {
            let native = format!("session-{index}");
            let parent = format!("session-{}", index.saturating_sub(1));
            let advisory = if index == 0 { &native } else { &parent };
            work.source_claims_examined += 1;
            assert!(!advisory_contradicts_child_local_claim_v0(
                &native, &parent, advisory, &mut work,
            ));
        }
        assert_eq!(work.source_claims_examined, SOURCES);
        assert_eq!(work.advisory_claims_examined, SOURCES);
        assert_eq!(work.identity_comparisons, SOURCES * 2 - 1);
    }
}
