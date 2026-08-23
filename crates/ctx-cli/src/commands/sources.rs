use std::path::PathBuf;

use anyhow::Result;
pub(crate) use ctx_cli_presentation::commands::sources::{SourcesArgs, SourcesCommand};

pub(crate) fn run_sources(
    mut args: SourcesArgs,
    data_root: PathBuf,
    telemetry: &mut crate::analytics::SourcesTelemetry,
    local_usage: &mut crate::local_usage::CliUsage,
    home_dir: Option<PathBuf>,
    automatic_provider_discovery: bool,
    provider_roots: Vec<ctx_history_cli::ProviderRootDefinition>,
    ui: &mut ctx_terminal::Ui,
) -> Result<()> {
    let Some(command) = args.command.take() else {
        return ctx_cli_presentation::commands::sources::run_sources(
            args,
            data_root,
            telemetry,
            local_usage,
            home_dir,
            automatic_provider_discovery,
            provider_roots,
            ui,
        );
    };
    if args.provider.is_some() || args.all || args.show_missing {
        anyhow::bail!("source listing filters cannot be combined with add or remove");
    }
    let operation = match &command {
        SourcesCommand::Add { .. } => "add",
        SourcesCommand::Remove { .. } => "remove",
    };
    let mutation = match command {
        SourcesCommand::Add {
            name,
            provider,
            root,
            scope,
        } => crate::config::add_provider_root(
            &data_root,
            &name,
            provider.capture_provider(),
            &root,
            scope.as_deref(),
        )?,
        SourcesCommand::Remove { name } => crate::config::remove_provider_root(&data_root, &name)?,
    };
    let value = serde_json::json!({
        "schema_version": 1,
        "operation": operation,
        "changed": mutation.changed,
        "root": {
            "name": mutation.root.id.clone(),
            "provider": mutation.root.provider.as_str(),
            "path": mutation.root.path.clone(),
            "scope": mutation.root.scope.clone(),
        }
    });
    if args.format.is_json() {
        ctx_terminal::print_json(value)?;
    } else {
        let document = crate::ui::Document::from_line(crate::ui::Line::text(format!(
            "{} provider root '{}' ({})",
            match (operation, mutation.changed) {
                ("add", true) => "Added",
                ("remove", true) => "Removed",
                (_, false) => "Kept",
                _ => "Updated",
            },
            mutation.root.id,
            mutation.root.path.display()
        )));
        ui.write_stdout(&document)?;
    }
    Ok(())
}
