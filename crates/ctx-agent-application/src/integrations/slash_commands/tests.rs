use std::fs;

use ctx_agent_integrations::slash_commands::{
    PathContext, SlashCommandAgent, SlashCommandInstallStatus,
};

use super::*;
use crate::IntegrationResultFact;

const PRODUCT: ProductIdentity<'static> = ProductIdentity {
    name: "ctx",
    version: "1.0.0-test",
};

#[test]
fn modified_copy_is_preserved_and_has_a_neutral_force_action() {
    let temp = tempfile::tempdir().unwrap();
    let context = PathContext::for_tests(temp.path().to_owned(), temp.path().to_owned());
    let command_path = temp.path().join(".gemini/commands/ctx-history.toml");
    fs::create_dir_all(command_path.parent().unwrap()).unwrap();
    fs::write(&command_path, "prompt = 'local'\n").unwrap();

    let outcome = install(
        SlashCommandInstallApplicationRequest {
            agents: vec![SlashCommandAgent::GeminiCli],
            all_agents: false,
            project: true,
            force: false,
        },
        &context,
        PRODUCT,
    )
    .unwrap();

    let result = &outcome.receipt.results[0];
    assert_eq!(result.status, SlashCommandInstallStatus::Modified);
    assert_eq!(
        fs::read_to_string(command_path).unwrap(),
        "prompt = 'local'\n"
    );
    assert_eq!(
        outcome.telemetry.result,
        Some(IntegrationResultFact::PartialError)
    );
    assert_eq!(
        force_install_command(PRODUCT, result).as_deref(),
        Some("ctx integrations install slash-commands --agent gemini-cli --force")
    );
}

#[test]
fn product_version_is_injected_into_installed_metadata() {
    let temp = tempfile::tempdir().unwrap();
    let context = PathContext::for_tests(temp.path().to_owned(), temp.path().to_owned());
    let outcome = install(
        SlashCommandInstallApplicationRequest {
            agents: vec![SlashCommandAgent::OpenCode],
            all_agents: false,
            project: true,
            force: false,
        },
        &context,
        PRODUCT,
    )
    .unwrap();

    assert_eq!(outcome.telemetry.result, Some(IntegrationResultFact::Ok));
    let metadata: serde_json::Value = serde_json::from_slice(
        &fs::read(
            temp.path()
                .join(".opencode/commands/.ctx-slash-commands.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(metadata["ctx_cli_version"], PRODUCT.version);
}
