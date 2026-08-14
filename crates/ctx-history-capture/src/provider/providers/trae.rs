pub(crate) use ctx_history_provider_trae::{
    trae_payload_admission, TraePayloadAdmission, TRAE_CHAT_KEYS, TRAE_CHAT_ROWS_QUERY,
    TRAE_SQLITE_VALUE_OVERHEAD_BYTES,
};

pub(crate) mod nativepath {
    pub(crate) type TraeReplacementTree = ctx_history_provider_trae::TraeReplacementTree<
        crate::provider::source_backed::family::CaptureProviderRuntime,
    >;
}
