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
    session_tree: bool,
}

impl CodexGenerationRouteV0 {
    pub(crate) fn participant(&self) -> usize {
        self.participant
    }

    pub(super) fn is_session_tree(&self) -> bool {
        self.session_tree
    }

    pub(super) fn prepared(&self) -> CodexSourceBackedResultV0<CodexPreparedRouteV0> {
        self.coordinator.prepared(self.participant)
    }

    pub(super) fn session_tree_roots(&self) -> CodexSourceBackedResultV0<Option<Arc<[PathBuf]>>> {
        match self.participant_definition()? {
            CodexGenerationParticipantV0::SessionTree { roots } => Ok(Some(roots)),
            CodexGenerationParticipantV0::ExplicitSession { .. } => Ok(None),
        }
    }

    pub(super) fn explicit_session_input(
        &self,
    ) -> CodexSourceBackedResultV0<Option<CodexExplicitSessionSourceBackedInputV0>> {
        match self.participant_definition()? {
            CodexGenerationParticipantV0::SessionTree { .. } => Ok(None),
            CodexGenerationParticipantV0::ExplicitSession { input } => Ok(Some(input)),
        }
    }

    fn participant_definition(&self) -> CodexSourceBackedResultV0<CodexGenerationParticipantV0> {
        self.coordinator
            .state
            .lock()
            .map_err(|_| CodexSourceBackedErrorV0::GenerationCoordinatorUnavailable)?
            .participants
            .get(&self.participant)
            .cloned()
            .ok_or(CodexSourceBackedErrorV0::GenerationCoordinatorUnavailable)
    }
}

#[derive(Clone)]
pub(super) struct CodexPreparedRouteV0 {
    pub(super) missing: bool,
    pub(super) sources: Vec<CodexSessionPlanV0>,
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
/// Shared JSONL selects routes and owns carried bases plus the physical
/// lifecycle. This provider-owned coordinator performs only route-local
/// inventory for Codex semantic execution. It never opens, parses, scans,
/// revalidates, or schedules an ancestor or descendant on behalf of a selected
/// leaf.
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
            session_tree: matches!(
                state.participants.get(&id),
                Some(CodexGenerationParticipantV0::SessionTree { .. })
            ),
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
            let (missing, discovered) = match discovery {
                CodexGenerationParticipantV0::SessionTree { roots } => {
                    let sources =
                        super::catalog::discover_codex_deferred_session_tree_inventory_v0(&roots)?;
                    (false, sources)
                }
                CodexGenerationParticipantV0::ExplicitSession { input } => {
                    let plan = observe_codex_explicit_session_source_backed_v0(&input)?;
                    (plan.is_none(), plan.into_iter().collect())
                }
            };

            let mut sources = Vec::with_capacity(discovered.len());
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
            routes.insert(participant, CodexPreparedRouteV0 { missing, sources });
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
