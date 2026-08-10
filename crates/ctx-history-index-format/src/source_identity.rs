use ctx_history_core::SourceKey;

pub fn source_token(source: &SourceKey) -> String {
    ctx_history_index_generation::hex(&source.identity().digest())
}

pub fn source_sort_key(source: &SourceKey) -> [u8; 32] {
    source.identity().digest()
}
