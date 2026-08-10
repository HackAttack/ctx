#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputObservationKind {
    Command,
    Tool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputOutcome {
    Success,
    Failure,
    Timeout,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputOutcomeMetadata {
    pub outcome: OutputOutcome,
    pub exit_code: Option<i32>,
    pub duration_ms: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_contract_debug_shapes_are_stable() {
        assert_eq!(format!("{:?}", OutputObservationKind::Command), "Command");
        assert_eq!(format!("{:?}", OutputOutcome::Timeout), "Timeout");
        assert_eq!(
            format!(
                "{:?}",
                OutputOutcomeMetadata {
                    outcome: OutputOutcome::Failure,
                    exit_code: Some(17),
                    duration_ms: Some(25),
                }
            ),
            "OutputOutcomeMetadata { outcome: Failure, exit_code: Some(17), duration_ms: Some(25) }"
        );
    }
}
