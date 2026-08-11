//! Content-free telemetry facts produced by integration workflows.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntegrationResultFact {
    Ok,
    PartialError,
    AllCurrent,
    NoneCurrent,
    PartiallyCurrent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetSelectionFact {
    Explicit,
    All,
    Picker,
    Detected,
    Fallback,
}

/// Closed, path-free facts for the CLI-owned integration telemetry envelope.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct IntegrationTelemetryFacts {
    pub selection: Option<TargetSelectionFact>,
    pub resolved_agents: Option<usize>,
    pub result: Option<IntegrationResultFact>,
    pub already_installed: Option<bool>,
    pub updated: Option<bool>,
    pub modified_targets: Option<usize>,
    pub current_targets: Option<usize>,
    pub missing_targets: Option<usize>,
    pub conflicting_targets: Option<usize>,
    pub invalid_targets: Option<usize>,
    pub unsupported_targets: Option<usize>,
}
