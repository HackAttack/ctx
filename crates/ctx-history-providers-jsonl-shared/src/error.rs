pub use ctx_history_provider_runtime::{CaptureError, ProviderJsonlInventoryLimit, Result};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_error_has_provider_runtime_identity_and_message() {
        let shared: CaptureError = ctx_history_provider_runtime::CaptureError::InvalidPayload(
            "literal fixture".to_owned(),
        );
        assert_eq!(
            shared.to_string(),
            "invalid capture payload: literal fixture"
        );

        let runtime: ctx_history_provider_runtime::CaptureError = shared;
        assert!(matches!(
            runtime,
            ctx_history_provider_runtime::CaptureError::InvalidPayload(detail)
                if detail == "literal fixture"
        ));
    }
}
