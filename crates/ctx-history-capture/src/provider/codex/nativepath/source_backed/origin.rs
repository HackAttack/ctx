#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum CodexOutcomeOriginV0 {
    CopiedFromAncestor { ancestor_native_session_id: String },
    Unproven,
}
