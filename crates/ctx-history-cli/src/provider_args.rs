use crate::HistoryProvider;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderArg(pub HistoryProvider);

impl ProviderArg {
    pub fn capture_provider(self) -> ctx_history_core::CaptureProvider {
        match self.0 {
            HistoryProvider::Custom => ctx_history_core::CaptureProvider::Custom,
            HistoryProvider::Native(value) => value
                .parse()
                .unwrap_or(ctx_history_core::CaptureProvider::Unknown),
        }
    }
}
