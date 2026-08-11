pub(crate) fn parse_since_filter(value: &str) -> anyhow::Result<chrono::DateTime<chrono::Utc>> {
    ctx_history_read_application::parse_since_filter(value).map_err(|error| {
        anyhow::anyhow!(error
            .to_string()
            .replacen("invalid since", "invalid --since", 1))
    })
}
