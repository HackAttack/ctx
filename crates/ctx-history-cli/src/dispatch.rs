pub fn rendered_cli_error() -> anyhow::Error {
    anyhow::Error::new(crate::RenderedCliError)
}
