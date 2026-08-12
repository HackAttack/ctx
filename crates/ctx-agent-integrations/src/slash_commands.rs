use std::{
    collections::BTreeMap,
    env, fs,
    path::{Component, Path, PathBuf},
};

use anyhow::{anyhow, Context, Result};
use ctx_history_core::utc_now;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::filesystem::atomic_update;

pub const COMMAND_NAME: &str = "ctx-history";
const METADATA_FILE: &str = ".ctx-slash-commands.json";

const COMMAND_INSTRUCTIONS: &str = r#"# ctx History

Use ctx to search local coding-agent history for this request.

User request: $ARGUMENTS

Search local agent history with `ctx`, prefer default text output for agent
reading, inspect cited events or sessions before making claims, and return a
concise answer with ctx citations. Use `--format json` only when piping to a script,
`jq`, or extracting exact machine fields.
"#;

const WINDSURF_WORKFLOW: &str = r#"# ctx History

Search local coding-agent history with ctx.

1. Treat any text after `/ctx-history` as the user request.
2. Search with `ctx search "<query>"` using default text output.
3. Inspect relevant citations with `ctx show event <id> --window 5` or `ctx show session <id>`.
4. Answer concisely and include ctx citations for claims based on local history.
5. Use `--format json` only when piping to a script, `jq`, or extracting exact machine fields.
"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SlashCommandAgent {
    Codex,
    GrokBuild,
    ClaudeCode,
    Cursor,
    OpenCode,
    MiMoCode,
    GeminiCli,
    QwenCode,
    Antigravity,
    GitHubCopilot,
    Pi,
    Goose,
    Continue,
    Windsurf,
}

impl SlashCommandAgent {
    pub const ALL: &'static [Self] = &[
        Self::Codex,
        Self::GrokBuild,
        Self::ClaudeCode,
        Self::Cursor,
        Self::OpenCode,
        Self::MiMoCode,
        Self::GeminiCli,
        Self::QwenCode,
        Self::Antigravity,
        Self::GitHubCopilot,
        Self::Pi,
        Self::Goose,
        Self::Continue,
        Self::Windsurf,
    ];

    const WRITABLE: &'static [Self] = &[
        Self::OpenCode,
        Self::MiMoCode,
        Self::GeminiCli,
        Self::QwenCode,
        Self::Windsurf,
    ];

    pub const fn id(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::GrokBuild => "grok-build",
            Self::ClaudeCode => "claude-code",
            Self::Cursor => "cursor",
            Self::OpenCode => "opencode",
            Self::MiMoCode => "mimocode",
            Self::GeminiCli => "gemini-cli",
            Self::QwenCode => "qwen-code",
            Self::Antigravity => "antigravity",
            Self::GitHubCopilot => "github-copilot",
            Self::Pi => "pi",
            Self::Goose => "goose",
            Self::Continue => "continue",
            Self::Windsurf => "windsurf",
        }
    }

    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::GrokBuild => "Grok Build",
            Self::ClaudeCode => "Claude Code",
            Self::Cursor => "Cursor",
            Self::OpenCode => "OpenCode",
            Self::MiMoCode => "MiMo Code",
            Self::GeminiCli => "Gemini CLI",
            Self::QwenCode => "Qwen Code",
            Self::Antigravity => "Antigravity",
            Self::GitHubCopilot => "GitHub Copilot",
            Self::Pi => "Pi",
            Self::Goose => "Goose",
            Self::Continue => "Continue",
            Self::Windsurf => "Windsurf",
        }
    }

    fn detected(self, context: &PathContext) -> bool {
        match self {
            Self::OpenCode => context.xdg_config_home.join("opencode").exists(),
            Self::MiMoCode => {
                context.mimocode_home.is_some()
                    || context.mimocode_config_dir.is_some()
                    || context.mimocode_config_dir().exists()
            }
            Self::GeminiCli => context.home.join(".gemini").exists(),
            Self::QwenCode => context.home.join(".qwen").exists(),
            Self::Windsurf => context.home.join(".codeium").join("windsurf").exists(),
            Self::Codex
            | Self::GrokBuild
            | Self::ClaudeCode
            | Self::Cursor
            | Self::Antigravity
            | Self::GitHubCopilot
            | Self::Pi
            | Self::Goose
            | Self::Continue => false,
        }
    }

    fn install_plan(self, project: bool, context: &PathContext) -> SlashCommandPlan {
        let file = |base_dir, filename, body| {
            SlashCommandPlan::File(CommandFileTarget {
                agent: self,
                scope: scope(project),
                base_dir,
                filename,
                body,
            })
        };
        match self {
            Self::OpenCode => file(
                if project {
                    context.cwd.join(".opencode").join("commands")
                } else {
                    context.xdg_config_home.join("opencode").join("commands")
                },
                format!("{COMMAND_NAME}.md"),
                opencode_command_body(),
            ),
            Self::MiMoCode => file(
                if project {
                    context.cwd.join(".mimocode").join("commands")
                } else {
                    context.mimocode_config_dir().join("commands")
                },
                format!("{COMMAND_NAME}.md"),
                opencode_command_body(),
            ),
            Self::GeminiCli => file(
                if project {
                    context.cwd.join(".gemini").join("commands")
                } else {
                    context.home.join(".gemini").join("commands")
                },
                format!("{COMMAND_NAME}.toml"),
                gemini_command_body(),
            ),
            Self::QwenCode => file(
                if project {
                    context.cwd.join(".qwen").join("commands")
                } else {
                    context.home.join(".qwen").join("commands")
                },
                format!("{COMMAND_NAME}.md"),
                qwen_command_body(),
            ),
            Self::Windsurf => file(
                if project {
                    context.cwd.join(".windsurf").join("workflows")
                } else {
                    context
                        .home
                        .join(".codeium")
                        .join("windsurf")
                        .join("global_workflows")
                },
                format!("{COMMAND_NAME}.md"),
                WINDSURF_WORKFLOW.to_owned(),
            ),
            Self::Codex
            | Self::GrokBuild
            | Self::ClaudeCode
            | Self::Cursor
            | Self::Antigravity => {
                SlashCommandPlan::SkillOnly {
                    agent: self,
                    note: "slash-style invocation is covered by Agent Skills; run `ctx integrations install skills --agent <agent>`",
                }
            }
            Self::GitHubCopilot | Self::Pi => SlashCommandPlan::SkillOnly {
                agent: self,
                note: "ctx supports this provider through the bundled Agent Skill; run `ctx integrations install skills --agent <agent>`",
            },
            Self::Goose => SlashCommandPlan::ManualOnly {
                agent: self,
                note: "Goose slash commands map to recipes in config.yaml; ctx does not edit that YAML safely yet",
            },
            Self::Continue => SlashCommandPlan::ManualOnly {
                agent: self,
                note: "Continue slash commands are invokable prompts referenced from config.yaml; ctx does not edit that YAML safely yet",
            },
        }
    }
}

#[derive(Debug, Clone)]
pub struct PathContext {
    home: PathBuf,
    xdg_config_home: PathBuf,
    cwd: PathBuf,
    mimocode_home: Option<PathBuf>,
    mimocode_config_dir: Option<PathBuf>,
}

impl PathContext {
    pub fn from_env() -> Result<Self> {
        let home = home_dir().context("resolve home directory")?;
        let xdg_config_home =
            non_empty_env_path("XDG_CONFIG_HOME").unwrap_or_else(|| home.join(".config"));
        Ok(Self {
            home,
            xdg_config_home,
            cwd: env::current_dir().context("resolve current directory")?,
            mimocode_home: non_empty_absolute_env_path("MIMOCODE_HOME")?,
            mimocode_config_dir: non_empty_env_path("MIMOCODE_CONFIG_DIR"),
        })
    }

    pub fn for_tests(home: PathBuf, cwd: PathBuf) -> Self {
        Self {
            xdg_config_home: home.join(".config"),
            home,
            cwd,
            mimocode_home: None,
            mimocode_config_dir: None,
        }
    }

    pub fn with_xdg_config_home(mut self, value: PathBuf) -> Self {
        self.xdg_config_home = value;
        self
    }

    fn mimocode_config_dir(&self) -> PathBuf {
        if let Some(path) = &self.mimocode_config_dir {
            return path.clone();
        }
        self.mimocode_home
            .as_ref()
            .map(|home| home.join("config"))
            .unwrap_or_else(|| self.xdg_config_home.join("mimocode"))
    }
}

#[derive(Debug, Clone)]
pub struct SlashCommandInstallRequest {
    pub agents: Vec<SlashCommandAgent>,
    pub all_agents: bool,
    pub project: bool,
    pub force: bool,
    pub product_version: String,
}

#[derive(Debug)]
pub struct SlashCommandInstallReceipt {
    pub project: bool,
    pub results: Vec<SlashCommandInstallResult>,
    pub failed: usize,
    pub already_installed: bool,
    pub updated: bool,
    pub modified_targets: usize,
}

#[derive(Debug, Clone, Copy)]
pub enum SlashCommandScope {
    Global,
    Project,
}

impl SlashCommandScope {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Project => "project",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlashCommandInstallStatus {
    Current,
    Stale,
    Modified,
    Missing,
    SkillOnly,
    ManualOnly,
}

impl SlashCommandInstallStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Stale => "stale",
            Self::Modified => "modified",
            Self::Missing => "missing",
            Self::SkillOnly => "skill_only",
            Self::ManualOnly => "manual_only",
        }
    }
}

#[derive(Debug)]
pub struct SlashCommandInstallResult {
    pub agent: SlashCommandAgent,
    pub scope: Option<SlashCommandScope>,
    pub path: Option<PathBuf>,
    pub success: bool,
    pub previous_status: SlashCommandInstallStatus,
    pub status: SlashCommandInstallStatus,
    pub already_installed: bool,
    pub updated: bool,
    pub error: Option<String>,
    pub note: Option<String>,
}

pub fn execute_install(
    request: SlashCommandInstallRequest,
    context: &PathContext,
) -> Result<SlashCommandInstallReceipt> {
    let agents = selected_agents(&request, context);
    let results = agents
        .into_iter()
        .map(|agent| {
            install_plan(
                agent.install_plan(request.project, context),
                request.force,
                &request.product_version,
            )
        })
        .collect::<Result<Vec<_>>>()?;
    let failed = results.iter().filter(|result| !result.success).count();
    let already_installed = !results.is_empty()
        && results.iter().all(|result| {
            result.already_installed
                || matches!(
                    result.status,
                    SlashCommandInstallStatus::SkillOnly | SlashCommandInstallStatus::ManualOnly
                )
        });
    Ok(SlashCommandInstallReceipt {
        project: request.project,
        failed,
        already_installed,
        updated: results.iter().any(|result| result.updated),
        modified_targets: results.iter().filter(|result| result.updated).count(),
        results,
    })
}

fn selected_agents(
    request: &SlashCommandInstallRequest,
    context: &PathContext,
) -> Vec<SlashCommandAgent> {
    if request.all_agents {
        return SlashCommandAgent::ALL.to_vec();
    }
    if !request.agents.is_empty() {
        return dedupe_agents(request.agents.iter().copied());
    }
    SlashCommandAgent::WRITABLE
        .iter()
        .copied()
        .filter(|agent| agent.detected(context))
        .collect()
}

fn dedupe_agents(agents: impl IntoIterator<Item = SlashCommandAgent>) -> Vec<SlashCommandAgent> {
    let mut deduped = Vec::new();
    for agent in agents {
        if !deduped.contains(&agent) {
            deduped.push(agent);
        }
    }
    deduped
}

#[derive(Debug, Clone)]
enum SlashCommandPlan {
    File(CommandFileTarget),
    SkillOnly {
        agent: SlashCommandAgent,
        note: &'static str,
    },
    ManualOnly {
        agent: SlashCommandAgent,
        note: &'static str,
    },
}

#[derive(Debug, Clone)]
struct CommandFileTarget {
    agent: SlashCommandAgent,
    scope: SlashCommandScope,
    base_dir: PathBuf,
    filename: String,
    body: String,
}

impl CommandFileTarget {
    fn command_path(&self) -> PathBuf {
        self.base_dir.join(&self.filename)
    }

    fn bundled_hash(&self) -> String {
        sha256_hex(self.body.as_bytes())
    }
}

#[derive(Debug)]
struct StatusResult {
    status: SlashCommandInstallStatus,
    metadata: Option<SlashCommandMetadata>,
    installed_hash: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SlashCommandMetadata {
    schema_version: u32,
    installer: String,
    command_name: String,
    files: BTreeMap<String, String>,
    ctx_cli_version: String,
    installed_at: String,
}

impl SlashCommandMetadata {
    fn current(target: &CommandFileTarget, product_version: &str) -> Self {
        Self {
            schema_version: 1,
            installer: "ctx-cli".to_owned(),
            command_name: COMMAND_NAME.to_owned(),
            files: BTreeMap::from([(target.filename.clone(), target.bundled_hash())]),
            ctx_cli_version: product_version.to_owned(),
            installed_at: utc_now().to_rfc3339(),
        }
    }
}

fn install_plan(
    plan: SlashCommandPlan,
    force: bool,
    product_version: &str,
) -> Result<SlashCommandInstallResult> {
    match plan {
        SlashCommandPlan::File(target) => install_file_target(&target, force, product_version),
        SlashCommandPlan::SkillOnly { agent, note } => Ok(SlashCommandInstallResult {
            agent,
            scope: None,
            path: None,
            success: true,
            previous_status: SlashCommandInstallStatus::SkillOnly,
            status: SlashCommandInstallStatus::SkillOnly,
            already_installed: true,
            updated: false,
            error: None,
            note: Some(note.replace("<agent>", agent.id())),
        }),
        SlashCommandPlan::ManualOnly { agent, note } => Ok(SlashCommandInstallResult {
            agent,
            scope: None,
            path: None,
            success: true,
            previous_status: SlashCommandInstallStatus::ManualOnly,
            status: SlashCommandInstallStatus::ManualOnly,
            already_installed: true,
            updated: false,
            error: None,
            note: Some(note.to_owned()),
        }),
    }
}

fn install_file_target(
    target: &CommandFileTarget,
    force: bool,
    product_version: &str,
) -> Result<SlashCommandInstallResult> {
    let previous = status_file_target(target)?;
    let bundled_hash = target.bundled_hash();
    if previous.installed_hash.as_deref() == Some(bundled_hash.as_str()) {
        if !metadata_is_current(target, previous.metadata.as_ref()) {
            write_metadata(target, product_version)?;
        }
        return Ok(SlashCommandInstallResult {
            agent: target.agent,
            scope: Some(target.scope),
            path: Some(target.command_path()),
            success: true,
            previous_status: previous.status,
            status: SlashCommandInstallStatus::Current,
            already_installed: true,
            updated: false,
            error: None,
            note: None,
        });
    }
    if previous.status == SlashCommandInstallStatus::Modified && !force {
        return Ok(SlashCommandInstallResult {
            agent: target.agent,
            scope: Some(target.scope),
            path: Some(target.command_path()),
            success: false,
            previous_status: previous.status,
            status: previous.status,
            already_installed: false,
            updated: false,
            error: Some("local command edits detected; rerun with --force to overwrite".to_owned()),
            note: None,
        });
    }
    write_command_file(target, product_version)?;
    Ok(SlashCommandInstallResult {
        agent: target.agent,
        scope: Some(target.scope),
        path: Some(target.command_path()),
        success: true,
        previous_status: previous.status,
        status: SlashCommandInstallStatus::Current,
        already_installed: false,
        updated: matches!(
            previous.status,
            SlashCommandInstallStatus::Stale | SlashCommandInstallStatus::Modified
        ),
        error: None,
        note: None,
    })
}

fn status_file_target(target: &CommandFileTarget) -> Result<StatusResult> {
    ensure_path_inside(&target.base_dir, &target.command_path())?;
    let command_path = target.command_path();
    let metadata = read_metadata(&target.base_dir);
    let installed_hash = match fs::symlink_metadata(&command_path) {
        Ok(metadata) if metadata.file_type().is_symlink() || metadata.is_dir() => None,
        Ok(_) => {
            Some(sha256_hex(&fs::read(&command_path).with_context(|| {
                format!("read {}", command_path.display())
            })?))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(error).with_context(|| format!("read {}", command_path.display()))
        }
    };
    let status = match installed_hash.as_deref() {
        None if command_path.exists() => SlashCommandInstallStatus::Modified,
        None => SlashCommandInstallStatus::Missing,
        Some(hash)
            if hash == target.bundled_hash()
                && metadata_manages_hash(target, metadata.as_ref(), hash) =>
        {
            SlashCommandInstallStatus::Current
        }
        Some(hash) if hash == target.bundled_hash() => SlashCommandInstallStatus::Stale,
        Some(hash) => match metadata
            .as_ref()
            .and_then(|metadata| metadata.files.get(&target.filename))
        {
            Some(metadata_hash) if metadata_hash == hash => SlashCommandInstallStatus::Stale,
            _ => SlashCommandInstallStatus::Modified,
        },
    };
    Ok(StatusResult {
        status,
        metadata,
        installed_hash,
    })
}

fn write_command_file(target: &CommandFileTarget, product_version: &str) -> Result<()> {
    ensure_path_inside(&target.base_dir, &target.command_path())?;
    atomic_update(&target.command_path(), |_| {
        Ok(target.body.as_bytes().to_vec())
    })
    .with_context(|| format!("write {}", target.command_path().display()))?;
    write_metadata(target, product_version)
}

fn write_metadata(target: &CommandFileTarget, product_version: &str) -> Result<()> {
    let metadata =
        serde_json::to_vec_pretty(&SlashCommandMetadata::current(target, product_version))?;
    let path = target.base_dir.join(METADATA_FILE);
    atomic_update(&path, |_| Ok(metadata)).with_context(|| format!("write {}", path.display()))
}

fn read_metadata(base_dir: &Path) -> Option<SlashCommandMetadata> {
    let body = fs::read(base_dir.join(METADATA_FILE)).ok()?;
    serde_json::from_slice(&body).ok()
}

fn metadata_is_current(
    target: &CommandFileTarget,
    metadata: Option<&SlashCommandMetadata>,
) -> bool {
    let hash = target.bundled_hash();
    metadata_manages_hash(target, metadata, &hash)
}

fn metadata_manages_hash(
    target: &CommandFileTarget,
    metadata: Option<&SlashCommandMetadata>,
    hash: &str,
) -> bool {
    metadata.is_some_and(|metadata| {
        metadata.schema_version == 1
            && metadata.installer == "ctx-cli"
            && metadata.command_name == COMMAND_NAME
            && metadata
                .files
                .get(&target.filename)
                .is_some_and(|metadata_hash| metadata_hash == hash)
    })
}

fn scope(project: bool) -> SlashCommandScope {
    if project {
        SlashCommandScope::Project
    } else {
        SlashCommandScope::Global
    }
}

fn home_dir() -> Option<PathBuf> {
    non_empty_env_path("HOME").or_else(|| non_empty_env_path("USERPROFILE"))
}

fn non_empty_env_path(key: &str) -> Option<PathBuf> {
    env::var_os(key)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn non_empty_absolute_env_path(key: &str) -> Result<Option<PathBuf>> {
    let Some(path) = non_empty_env_path(key) else {
        return Ok(None);
    };
    if !path.is_absolute() {
        return Err(anyhow!(
            "{key} must be an absolute path: {}",
            path.display()
        ));
    }
    Ok(Some(path))
}

fn opencode_command_body() -> String {
    format!(
        "---\ndescription: Search local agent history with ctx\nargument-hint: [question or topic]\n---\n\n{COMMAND_INSTRUCTIONS}"
    )
}

fn gemini_command_body() -> String {
    let prompt = COMMAND_INSTRUCTIONS.replace("$ARGUMENTS", "{{args}}");
    format!(
        "description = \"{}\"\nprompt = '''\n{}'''\n",
        toml_basic_string("Search local agent history with ctx"),
        prompt
    )
}

fn qwen_command_body() -> String {
    let prompt = COMMAND_INSTRUCTIONS.replace("$ARGUMENTS", "{{args}}");
    format!("---\ndescription: Search local agent history with ctx\n---\n\n{prompt}")
}

fn toml_basic_string(value: &str) -> String {
    value
        .chars()
        .flat_map(|ch| match ch {
            '\\' => "\\\\".chars().collect::<Vec<_>>(),
            '"' => "\\\"".chars().collect::<Vec<_>>(),
            '\n' => "\\n".chars().collect::<Vec<_>>(),
            '\r' => "\\r".chars().collect::<Vec<_>>(),
            '\t' => "\\t".chars().collect::<Vec<_>>(),
            other => vec![other],
        })
        .collect()
}

fn ensure_path_inside(base: &Path, target: &Path) -> Result<()> {
    if has_parent_component(base) || has_parent_component(target) {
        return Err(anyhow!("slash command path contains parent traversal"));
    }
    if !target.starts_with(base) {
        return Err(anyhow!("slash command path escapes target directory"));
    }
    Ok(())
}

fn has_parent_component(path: &Path) -> bool {
    path.components()
        .any(|component| matches!(component, Component::ParentDir))
}

fn sha256_hex(body: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(body);
    format!("sha256:{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PRODUCT_VERSION: &str = "1.0.0-test";

    fn request(agent: SlashCommandAgent) -> SlashCommandInstallRequest {
        SlashCommandInstallRequest {
            agents: vec![agent],
            all_agents: false,
            project: true,
            force: false,
            product_version: PRODUCT_VERSION.to_owned(),
        }
    }

    #[test]
    fn detected_file_targets_are_selected_once_and_in_order() {
        let root = tempfile::tempdir().unwrap();
        let xdg = root.path().join("xdg");
        fs::create_dir_all(xdg.join("opencode")).unwrap();
        fs::create_dir_all(xdg.join("mimocode")).unwrap();
        let context = PathContext::for_tests(root.path().to_owned(), root.path().to_owned())
            .with_xdg_config_home(xdg);
        let request = SlashCommandInstallRequest {
            agents: Vec::new(),
            all_agents: false,
            project: false,
            force: false,
            product_version: PRODUCT_VERSION.to_owned(),
        };

        assert_eq!(
            selected_agents(&request, &context),
            vec![SlashCommandAgent::OpenCode, SlashCommandAgent::MiMoCode]
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    #[test]
    fn managed_file_is_idempotent_and_refreshes_stale_content() {
        let root = tempfile::tempdir().unwrap();
        let context = PathContext::for_tests(root.path().to_owned(), root.path().to_owned());
        let request = request(SlashCommandAgent::OpenCode);
        let first = execute_install(request.clone(), &context).unwrap();
        assert_eq!(
            first.results[0].previous_status,
            SlashCommandInstallStatus::Missing
        );
        assert!(!first.results[0].already_installed);

        let second = execute_install(request.clone(), &context).unwrap();
        assert!(second.results[0].already_installed);

        let target = match SlashCommandAgent::OpenCode.install_plan(true, &context) {
            SlashCommandPlan::File(target) => target,
            _ => unreachable!(),
        };
        let old_body = "---\ndescription: old\n---\n\nold\n";
        fs::write(target.command_path(), old_body).unwrap();
        let mut metadata = SlashCommandMetadata::current(&target, PRODUCT_VERSION);
        metadata
            .files
            .insert(target.filename.clone(), sha256_hex(old_body.as_bytes()));
        fs::write(
            target.base_dir.join(METADATA_FILE),
            serde_json::to_vec_pretty(&metadata).unwrap(),
        )
        .unwrap();

        let refreshed = execute_install(request, &context).unwrap();
        assert_eq!(
            refreshed.results[0].previous_status,
            SlashCommandInstallStatus::Stale
        );
        assert!(refreshed.results[0].updated);
    }

    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    #[test]
    fn local_command_edits_require_force_and_unrelated_files_survive() {
        let root = tempfile::tempdir().unwrap();
        let context = PathContext::for_tests(root.path().to_owned(), root.path().to_owned());
        let mut request = request(SlashCommandAgent::GeminiCli);
        let target = match SlashCommandAgent::GeminiCli.install_plan(true, &context) {
            SlashCommandPlan::File(target) => target,
            _ => unreachable!(),
        };
        fs::create_dir_all(&target.base_dir).unwrap();
        fs::write(target.command_path(), "prompt = 'local'\n").unwrap();
        fs::write(target.base_dir.join("keep.txt"), "keep").unwrap();

        let skipped = execute_install(request.clone(), &context).unwrap();
        assert!(!skipped.results[0].success);
        assert_eq!(
            skipped.results[0].status,
            SlashCommandInstallStatus::Modified
        );

        request.force = true;
        let forced = execute_install(request, &context).unwrap();
        assert!(forced.results[0].success);
        assert_eq!(
            fs::read_to_string(target.base_dir.join("keep.txt")).unwrap(),
            "keep"
        );
    }

    #[test]
    fn generated_command_bytes_match_the_public_contract() {
        assert_eq!(
            opencode_command_body(),
            format!(
                "---\ndescription: Search local agent history with ctx\nargument-hint: [question or topic]\n---\n\n{COMMAND_INSTRUCTIONS}"
            )
        );
        assert!(gemini_command_body().contains("User request: {{args}}"));
        assert!(qwen_command_body().ends_with(
            COMMAND_INSTRUCTIONS
                .replace("$ARGUMENTS", "{{args}}")
                .as_str()
        ));
        assert!(WINDSURF_WORKFLOW.ends_with("machine fields.\n"));
    }

    #[test]
    fn skill_only_agents_do_not_write_legacy_prompts() {
        let root = tempfile::tempdir().unwrap();
        let context = PathContext::for_tests(root.path().to_owned(), root.path().to_owned());
        let receipt = execute_install(request(SlashCommandAgent::Codex), &context).unwrap();
        assert_eq!(
            receipt.results[0].status,
            SlashCommandInstallStatus::SkillOnly
        );
        assert!(!root.path().join(".codex").join("prompts").exists());
    }

    #[test]
    fn grok_build_is_skill_only_and_writes_no_command_file() {
        let root = tempfile::tempdir().unwrap();
        let context = PathContext::for_tests(root.path().to_owned(), root.path().to_owned());
        let receipt = execute_install(request(SlashCommandAgent::GrokBuild), &context).unwrap();

        assert_eq!(receipt.results[0].agent.id(), "grok-build");
        assert_eq!(receipt.results[0].agent.display_name(), "Grok Build");
        assert_eq!(
            receipt.results[0].status,
            SlashCommandInstallStatus::SkillOnly
        );
        assert!(receipt.results[0].path.is_none());
        assert!(!root.path().join(".grok").exists());
    }

    #[test]
    fn interrupted_content_then_metadata_publication_is_stale_and_recoverable() {
        let root = tempfile::tempdir().unwrap();
        let context = PathContext::for_tests(root.path().to_owned(), root.path().to_owned());
        let request = request(SlashCommandAgent::OpenCode);
        let target = match SlashCommandAgent::OpenCode.install_plan(true, &context) {
            SlashCommandPlan::File(target) => target,
            _ => unreachable!(),
        };
        fs::create_dir_all(&target.base_dir).unwrap();
        fs::create_dir(target.base_dir.join(METADATA_FILE)).unwrap();

        let error = execute_install(request.clone(), &context).unwrap_err();
        assert!(format!("{error:#}").contains("non-regular file"));
        assert_eq!(
            fs::read(target.command_path()).unwrap(),
            target.body.as_bytes()
        );
        assert_eq!(
            status_file_target(&target).unwrap().status,
            SlashCommandInstallStatus::Stale
        );

        fs::remove_dir(target.base_dir.join(METADATA_FILE)).unwrap();
        let repaired = execute_install(request, &context).unwrap();
        assert!(repaired.results[0].already_installed);
        assert!(!repaired.results[0].updated);
        assert_eq!(
            status_file_target(&target).unwrap().status,
            SlashCommandInstallStatus::Current
        );
    }
}
