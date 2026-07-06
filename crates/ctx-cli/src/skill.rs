use std::{
    collections::BTreeMap,
    env, fs,
    io::{self, IsTerminal, Write},
    path::{Component, Path, PathBuf},
};

use anyhow::{anyhow, Context, Result};
use clap::{Args, Subcommand, ValueEnum};
use ctx_history_core::utc_now;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::{analytics, AnalyticsProperties};

const BUNDLED_SKILL_NAME: &str = "ctx-agent-history-search";
const BUNDLED_SKILL_BODY: &str = include_str!("../../../skills/ctx-agent-history-search/SKILL.md");
const METADATA_FILE: &str = ".ctx-skill.json";

#[derive(Debug, Args)]
pub(crate) struct SkillArgs {
    #[command(subcommand)]
    command: SkillCommand,
}

#[derive(Debug, Subcommand)]
enum SkillCommand {
    #[command(about = "Install or refresh the bundled ctx agent-history skill")]
    Install(SkillInstallArgs),
    #[command(about = "Check whether the bundled ctx agent-history skill is installed")]
    Status(SkillStatusArgs),
}

#[derive(Debug, Args)]
struct SkillInstallArgs {
    #[arg(long = "agent", value_enum, conflicts_with = "all_agents")]
    agent: Vec<SkillAgentArg>,
    #[arg(long, conflicts_with = "agent")]
    all_agents: bool,
    #[arg(
        long,
        help = "Install into the current project instead of global agent dirs"
    )]
    project: bool,
    #[arg(long)]
    json: bool,
    #[arg(long, help = "Overwrite locally modified bundled skill files")]
    force: bool,
}

#[derive(Debug, Args)]
struct SkillStatusArgs {
    #[arg(long = "agent", value_enum, conflicts_with = "all_agents")]
    agent: Vec<SkillAgentArg>,
    #[arg(long, conflicts_with = "agent")]
    all_agents: bool,
    #[arg(
        long,
        help = "Check the current project's skill dirs instead of global dirs"
    )]
    project: bool,
    #[arg(long)]
    json: bool,
}

impl SkillArgs {
    pub(crate) fn json_output(&self) -> bool {
        match &self.command {
            SkillCommand::Install(args) => args.json,
            SkillCommand::Status(args) => args.json,
        }
    }

    pub(crate) fn add_initial_analytics(&self, properties: &mut AnalyticsProperties) {
        analytics::insert_str(properties, "skill_name", BUNDLED_SKILL_NAME);
        match &self.command {
            SkillCommand::Install(args) => {
                analytics::insert_str(properties, "skill_action", "install");
                insert_target_analytics(properties, &args.agent, args.all_agents, args.project);
            }
            SkillCommand::Status(args) => {
                analytics::insert_str(properties, "skill_action", "status");
                insert_target_analytics(properties, &args.agent, args.all_agents, args.project);
            }
        }
    }
}

fn insert_target_analytics(
    properties: &mut AnalyticsProperties,
    agents: &[SkillAgentArg],
    all_agents: bool,
    project: bool,
) {
    analytics::insert_str(
        properties,
        "skill_scope",
        if project { "project" } else { "global" },
    );
    analytics::insert_str(
        properties,
        "target_agent_group",
        if all_agents {
            "all"
        } else if agents.is_empty() {
            "default"
        } else {
            "explicit"
        },
    );
    let count = if all_agents {
        SkillAgentArg::ALL.len()
    } else {
        agents.len().max(1)
    };
    analytics::insert_count_bucket(properties, "target_agents_count_bucket", count as u64);
}

pub(crate) fn run(args: SkillArgs, analytics_properties: &mut AnalyticsProperties) -> Result<()> {
    let context = PathContext::from_env()?;
    match args.command {
        SkillCommand::Install(args) => run_install(args, &context, analytics_properties),
        SkillCommand::Status(args) => run_status(args, &context, analytics_properties),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, ValueEnum)]
enum SkillAgentArg {
    #[value(name = "universal", alias = "agents")]
    Universal,
    Codex,
    #[value(name = "claude-code", alias = "claude")]
    ClaudeCode,
    Cursor,
    #[value(name = "opencode", alias = "open-code")]
    OpenCode,
    #[value(name = "xdg-agents", alias = "config-agents")]
    XdgAgents,
    #[value(name = "gemini-cli", alias = "gemini")]
    GeminiCli,
    Antigravity,
    #[value(name = "antigravity-cli")]
    AntigravityCli,
    #[value(name = "github-copilot", alias = "copilot")]
    GitHubCopilot,
    Pi,
    Goose,
}

impl SkillAgentArg {
    const ALL: &'static [Self] = &[
        Self::Universal,
        Self::Codex,
        Self::ClaudeCode,
        Self::Cursor,
        Self::OpenCode,
        Self::XdgAgents,
        Self::GeminiCli,
        Self::Antigravity,
        Self::AntigravityCli,
        Self::GitHubCopilot,
        Self::Pi,
        Self::Goose,
    ];

    fn id(self) -> &'static str {
        match self {
            Self::Universal => "universal",
            Self::Codex => "codex",
            Self::ClaudeCode => "claude-code",
            Self::Cursor => "cursor",
            Self::OpenCode => "opencode",
            Self::XdgAgents => "xdg-agents",
            Self::GeminiCli => "gemini-cli",
            Self::Antigravity => "antigravity",
            Self::AntigravityCli => "antigravity-cli",
            Self::GitHubCopilot => "github-copilot",
            Self::Pi => "pi",
            Self::Goose => "goose",
        }
    }

    fn display_name(self) -> &'static str {
        match self {
            Self::Universal => "Universal .agents",
            Self::Codex => "Codex",
            Self::ClaudeCode => "Claude Code",
            Self::Cursor => "Cursor",
            Self::OpenCode => "OpenCode",
            Self::XdgAgents => "XDG agents",
            Self::GeminiCli => "Gemini CLI",
            Self::Antigravity => "Antigravity",
            Self::AntigravityCli => "Antigravity CLI",
            Self::GitHubCopilot => "GitHub Copilot",
            Self::Pi => "Pi",
            Self::Goose => "Goose",
        }
    }

    fn project_skills_dir(self) -> &'static str {
        match self {
            Self::ClaudeCode => ".claude/skills",
            Self::Pi => ".pi/skills",
            Self::Goose => ".goose/skills",
            Self::Universal
            | Self::Codex
            | Self::Cursor
            | Self::OpenCode
            | Self::XdgAgents
            | Self::GeminiCli
            | Self::Antigravity
            | Self::AntigravityCli
            | Self::GitHubCopilot => ".agents/skills",
        }
    }

    fn global_skills_dir(self, context: &PathContext) -> PathBuf {
        match self {
            Self::Universal => context.home.join(".agents").join("skills"),
            Self::Codex => context
                .env_or_home_child("CODEX_HOME", ".codex")
                .join("skills"),
            Self::ClaudeCode => context
                .env_or_home_child("CLAUDE_CONFIG_DIR", ".claude")
                .join("skills"),
            Self::Cursor => context.home.join(".cursor").join("skills"),
            Self::OpenCode => context.xdg_config_home.join("opencode").join("skills"),
            Self::XdgAgents => context.xdg_config_home.join("agents").join("skills"),
            Self::GeminiCli => context.home.join(".gemini").join("skills"),
            Self::Antigravity => context
                .home
                .join(".gemini")
                .join("antigravity")
                .join("skills"),
            Self::AntigravityCli => context
                .home
                .join(".gemini")
                .join("antigravity-cli")
                .join("skills"),
            Self::GitHubCopilot => context.home.join(".copilot").join("skills"),
            Self::Pi => context.home.join(".pi").join("agent").join("skills"),
            Self::Goose => context.xdg_config_home.join("goose").join("skills"),
        }
    }

    fn needs_agent_specific_default(self) -> bool {
        self.project_skills_dir() != ".agents/skills"
    }

    fn detect_dir(self, context: &PathContext) -> Option<PathBuf> {
        match self {
            Self::Universal => Some(context.home.join(".agents")),
            Self::Codex => Some(context.env_or_home_child("CODEX_HOME", ".codex")),
            Self::ClaudeCode => Some(context.env_or_home_child("CLAUDE_CONFIG_DIR", ".claude")),
            Self::Cursor => Some(context.home.join(".cursor")),
            Self::OpenCode => Some(context.xdg_config_home.join("opencode")),
            Self::XdgAgents => Some(context.xdg_config_home.join("agents")),
            Self::GeminiCli => Some(context.home.join(".gemini")),
            Self::Antigravity => Some(context.home.join(".gemini").join("antigravity")),
            Self::AntigravityCli => Some(context.home.join(".gemini").join("antigravity-cli")),
            Self::GitHubCopilot => Some(context.home.join(".copilot")),
            Self::Pi => Some(context.home.join(".pi").join("agent")),
            Self::Goose => Some(context.xdg_config_home.join("goose")),
        }
    }
}

#[derive(Debug, Clone)]
struct PathContext {
    home: PathBuf,
    xdg_config_home: PathBuf,
    cwd: PathBuf,
    env_overrides: BTreeMap<String, PathBuf>,
}

impl PathContext {
    fn from_env() -> Result<Self> {
        let home = home_dir().context("resolve home directory")?;
        let xdg_config_home =
            non_empty_env_path("XDG_CONFIG_HOME").unwrap_or_else(|| home.join(".config"));
        let mut env_overrides = BTreeMap::new();
        for key in ["CODEX_HOME", "CLAUDE_CONFIG_DIR"] {
            if let Some(path) = non_empty_env_path(key) {
                env_overrides.insert(key.to_owned(), path);
            }
        }
        Ok(Self {
            home,
            xdg_config_home,
            cwd: env::current_dir().context("resolve current directory")?,
            env_overrides,
        })
    }

    #[cfg(test)]
    fn for_tests(home: PathBuf, cwd: PathBuf) -> Self {
        Self {
            xdg_config_home: home.join(".config"),
            home,
            cwd,
            env_overrides: BTreeMap::new(),
        }
    }

    #[cfg(test)]
    fn with_env_override(mut self, key: &str, value: PathBuf) -> Self {
        self.env_overrides.insert(key.to_owned(), value);
        self
    }

    #[cfg(test)]
    fn with_xdg_config_home(mut self, value: PathBuf) -> Self {
        self.xdg_config_home = value;
        self
    }

    fn env_or_home_child(&self, key: &str, fallback_child: &str) -> PathBuf {
        self.env_overrides
            .get(key)
            .cloned()
            .unwrap_or_else(|| self.home.join(fallback_child))
    }

    fn agent_detected(&self, agent: SkillAgentArg) -> bool {
        if agent == SkillAgentArg::Codex
            && !self.env_overrides.contains_key("CODEX_HOME")
            && Path::new("/etc/codex").exists()
        {
            return true;
        }
        agent.detect_dir(self).is_some_and(|path| path.exists())
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

#[derive(Debug, Clone)]
struct SkillTarget {
    agent: SkillAgentArg,
    scope: SkillScope,
    base_dir: PathBuf,
    skill_dir: PathBuf,
}

#[derive(Debug, Clone, Copy)]
enum SkillScope {
    Global,
    Project,
}

impl SkillScope {
    fn as_str(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Project => "project",
        }
    }
}

#[cfg(test)]
fn explicit_selected_agents(
    agents: &[SkillAgentArg],
    all_agents: bool,
) -> Option<Vec<SkillAgentArg>> {
    if all_agents {
        Some(SkillAgentArg::ALL.to_vec())
    } else if agents.is_empty() {
        None
    } else {
        Some(dedupe_agents(agents.iter().copied()))
    }
}

fn dedupe_agents(agents: impl IntoIterator<Item = SkillAgentArg>) -> Vec<SkillAgentArg> {
    let mut deduped = Vec::new();
    for agent in agents {
        if !deduped.contains(&agent) {
            deduped.push(agent);
        }
    }
    deduped
}

fn detected_agents(context: &PathContext) -> Vec<SkillAgentArg> {
    picker_agents()
        .iter()
        .copied()
        .filter(|agent| context.agent_detected(*agent))
        .collect()
}

fn detected_agent_specific_agents(context: &PathContext) -> Vec<SkillAgentArg> {
    detected_agents(context)
        .into_iter()
        .filter(|agent| agent.needs_agent_specific_default())
        .collect()
}

fn default_noninteractive_agents(
    context: &PathContext,
) -> (Vec<SkillAgentArg>, SkillSelectionSource) {
    let mut agents = vec![SkillAgentArg::Universal];
    let detected_specific = detected_agent_specific_agents(context);
    let source = if detected_specific.is_empty() {
        SkillSelectionSource::Fallback
    } else {
        agents.extend(detected_specific);
        SkillSelectionSource::Detected
    };
    (agents, source)
}

fn default_picker_agents(context: &PathContext) -> Vec<SkillAgentArg> {
    let (agents, _) = default_noninteractive_agents(context);
    agents
}

fn picker_agents() -> &'static [SkillAgentArg] {
    &[
        SkillAgentArg::Universal,
        SkillAgentArg::ClaudeCode,
        SkillAgentArg::Codex,
        SkillAgentArg::Cursor,
        SkillAgentArg::OpenCode,
        SkillAgentArg::GeminiCli,
        SkillAgentArg::Antigravity,
        SkillAgentArg::AntigravityCli,
        SkillAgentArg::GitHubCopilot,
        SkillAgentArg::Pi,
        SkillAgentArg::Goose,
        SkillAgentArg::XdgAgents,
    ]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SkillSelectionSource {
    Explicit,
    All,
    Picker,
    Detected,
    Fallback,
}

impl SkillSelectionSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::All => "all",
            Self::Picker => "picker",
            Self::Detected => "detected",
            Self::Fallback => "fallback",
        }
    }
}

#[derive(Debug, Clone)]
struct SkillAgentSelection {
    agents: Vec<SkillAgentArg>,
    source: SkillSelectionSource,
}

fn install_agent_selection(
    args: &SkillInstallArgs,
    context: &PathContext,
) -> Result<SkillAgentSelection> {
    if args.all_agents {
        return Ok(SkillAgentSelection {
            agents: SkillAgentArg::ALL.to_vec(),
            source: SkillSelectionSource::All,
        });
    }
    if !args.agent.is_empty() {
        return Ok(SkillAgentSelection {
            agents: dedupe_agents(args.agent.iter().copied()),
            source: SkillSelectionSource::Explicit,
        });
    }
    if args.json || !can_prompt() {
        let (agents, source) = default_noninteractive_agents(context);
        return Ok(SkillAgentSelection { agents, source });
    }
    let agents = prompt_for_agents(context)?;
    Ok(SkillAgentSelection {
        agents,
        source: SkillSelectionSource::Picker,
    })
}

fn status_agent_selection(args: &SkillStatusArgs, context: &PathContext) -> SkillAgentSelection {
    if args.all_agents {
        return SkillAgentSelection {
            agents: SkillAgentArg::ALL.to_vec(),
            source: SkillSelectionSource::All,
        };
    }
    if !args.agent.is_empty() {
        return SkillAgentSelection {
            agents: dedupe_agents(args.agent.iter().copied()),
            source: SkillSelectionSource::Explicit,
        };
    }
    let (agents, source) = default_noninteractive_agents(context);
    SkillAgentSelection { agents, source }
}

fn can_prompt() -> bool {
    io::stdin().is_terminal() && io::stderr().is_terminal()
}

fn prompt_for_agents(context: &PathContext) -> Result<Vec<SkillAgentArg>> {
    let options = picker_agents();
    let detected = detected_agents(context);
    let defaults = default_picker_agents(context);
    let mut stderr = io::stderr();
    writeln!(
        stderr,
        "Select where to install {BUNDLED_SKILL_NAME}. Detected agents are preselected."
    )?;
    writeln!(
        stderr,
        "Press Enter for the marked defaults, or enter numbers like 1,2."
    )?;
    for (index, agent) in options.iter().enumerate() {
        let marker = if defaults.contains(agent) { "*" } else { " " };
        let detected_hint = if detected.contains(agent) {
            " detected"
        } else {
            ""
        };
        let target = single_target(*agent, false, context)?;
        writeln!(
            stderr,
            "  {}. [{}] {} -> {}{}",
            index + 1,
            marker,
            agent.display_name(),
            target.skill_dir.display(),
            detected_hint
        )?;
    }
    loop {
        write!(stderr, "Install target(s): ")?;
        stderr.flush()?;
        let mut line = String::new();
        io::stdin()
            .read_line(&mut line)
            .context("read skill install selection")?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return Ok(defaults);
        }
        if matches!(
            trimmed.to_ascii_lowercase().as_str(),
            "q" | "quit" | "cancel"
        ) {
            return Err(anyhow!("skill install canceled"));
        }
        match parse_picker_selection(trimmed, options) {
            Ok(agents) => return Ok(agents),
            Err(err) => {
                writeln!(stderr, "{err}")?;
            }
        }
    }
}

fn parse_picker_selection(input: &str, options: &[SkillAgentArg]) -> Result<Vec<SkillAgentArg>> {
    let input = input.trim();
    if input.eq_ignore_ascii_case("all") {
        return Ok(options.to_vec());
    }
    let mut selected = Vec::new();
    for raw in input
        .split([',', ' ', '\t'])
        .filter(|part| !part.trim().is_empty())
    {
        let token = raw.trim();
        let agent = if let Ok(index) = token.parse::<usize>() {
            options
                .get(index.saturating_sub(1))
                .copied()
                .ok_or_else(|| anyhow!("invalid selection {token}: choose 1-{}", options.len()))?
        } else {
            agent_from_name(token).ok_or_else(|| anyhow!("unknown agent: {token}"))?
        };
        if !selected.contains(&agent) {
            selected.push(agent);
        }
    }
    if selected.is_empty() {
        return Err(anyhow!("choose at least one install target"));
    }
    Ok(selected)
}

fn agent_from_name(value: &str) -> Option<SkillAgentArg> {
    match value.to_ascii_lowercase().as_str() {
        "universal" | "agents" | ".agents" => Some(SkillAgentArg::Universal),
        "codex" => Some(SkillAgentArg::Codex),
        "claude" | "claude-code" | "claudecode" => Some(SkillAgentArg::ClaudeCode),
        "cursor" => Some(SkillAgentArg::Cursor),
        "opencode" | "open-code" => Some(SkillAgentArg::OpenCode),
        "xdg-agents" | "config-agents" => Some(SkillAgentArg::XdgAgents),
        "gemini" | "gemini-cli" => Some(SkillAgentArg::GeminiCli),
        "antigravity" => Some(SkillAgentArg::Antigravity),
        "antigravity-cli" => Some(SkillAgentArg::AntigravityCli),
        "github-copilot" | "copilot" => Some(SkillAgentArg::GitHubCopilot),
        "pi" => Some(SkillAgentArg::Pi),
        "goose" => Some(SkillAgentArg::Goose),
        _ => None,
    }
}

fn single_target(
    agent: SkillAgentArg,
    project: bool,
    context: &PathContext,
) -> Result<SkillTarget> {
    let skill_name = sanitize_skill_name(BUNDLED_SKILL_NAME)?;
    let (scope, base_dir) = if project {
        (
            SkillScope::Project,
            context.cwd.join(agent.project_skills_dir()),
        )
    } else {
        (SkillScope::Global, agent.global_skills_dir(context))
    };
    let skill_dir = base_dir.join(&skill_name);
    ensure_path_inside(&base_dir, &skill_dir)
        .with_context(|| format!("resolve {} skill path", agent.id()))?;
    Ok(SkillTarget {
        agent,
        scope,
        base_dir,
        skill_dir,
    })
}

#[cfg(test)]
fn resolve_targets(
    agents: &[SkillAgentArg],
    all_agents: bool,
    project: bool,
    context: &PathContext,
) -> Result<Vec<SkillTarget>> {
    let selected = explicit_selected_agents(agents, all_agents)
        .unwrap_or_else(|| vec![SkillAgentArg::Universal]);
    resolve_targets_for_agents(&selected, project, context)
}

fn resolve_targets_for_agents(
    agents: &[SkillAgentArg],
    project: bool,
    context: &PathContext,
) -> Result<Vec<SkillTarget>> {
    agents
        .iter()
        .copied()
        .map(|agent| single_target(agent, project, context))
        .collect()
}

fn run_install(
    args: SkillInstallArgs,
    context: &PathContext,
    analytics_properties: &mut AnalyticsProperties,
) -> Result<()> {
    let selection = install_agent_selection(&args, context)?;
    insert_selection_analytics(analytics_properties, &selection);
    let targets = resolve_targets_for_agents(&selection.agents, args.project, context)?;
    let mut results = Vec::with_capacity(targets.len());
    for target in &targets {
        results.push(install_target(target, args.force)?);
    }
    let failed = results.iter().filter(|result| !result.success).count();
    let already_installed = results.iter().all(|result| result.already_installed);
    let updated = results.iter().any(|result| result.updated);
    analytics::insert_str(
        analytics_properties,
        "install_result",
        if failed == 0 { "ok" } else { "partial_error" },
    );
    analytics::insert_bool(analytics_properties, "already_installed", already_installed);
    analytics::insert_bool(analytics_properties, "updated", updated);
    if args.json {
        println!(
            "{}",
            json!({
                "skill": BUNDLED_SKILL_NAME,
                "scope": if args.project { "project" } else { "global" },
                "results": results.iter().map(InstallResult::to_json).collect::<Vec<_>>(),
            })
        );
    } else {
        print_install_results(&results);
    }
    if failed > 0 {
        return Err(anyhow!("failed to install skill for {failed} target(s)"));
    }
    Ok(())
}

fn run_status(
    args: SkillStatusArgs,
    context: &PathContext,
    analytics_properties: &mut AnalyticsProperties,
) -> Result<()> {
    let selection = status_agent_selection(&args, context);
    insert_selection_analytics(analytics_properties, &selection);
    let targets = resolve_targets_for_agents(&selection.agents, args.project, context)?;
    let results = targets
        .iter()
        .map(status_target)
        .collect::<Result<Vec<_>>>()?;
    let current_count = results
        .iter()
        .filter(|result| result.status == SkillInstallStatus::Current)
        .count();
    analytics::insert_str(
        analytics_properties,
        "status_result",
        if current_count == results.len() {
            "all_current"
        } else if current_count == 0 {
            "none_current"
        } else {
            "partially_current"
        },
    );
    analytics::insert_count_bucket(
        analytics_properties,
        "current_targets_bucket",
        current_count as u64,
    );
    if args.json {
        println!(
            "{}",
            json!({
                "skill": BUNDLED_SKILL_NAME,
                "scope": if args.project { "project" } else { "global" },
                "results": results.iter().map(StatusResult::to_json).collect::<Vec<_>>(),
            })
        );
    } else {
        print_status_results(&results);
    }
    Ok(())
}

fn insert_selection_analytics(
    analytics_properties: &mut AnalyticsProperties,
    selection: &SkillAgentSelection,
) {
    analytics::insert_str(
        analytics_properties,
        "target_agent_group",
        selection.source.as_str(),
    );
    analytics::insert_count_bucket(
        analytics_properties,
        "target_agents_count_bucket",
        selection.agents.len() as u64,
    );
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SkillInstallStatus {
    Current,
    Stale,
    Modified,
    Missing,
}

impl SkillInstallStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Stale => "stale",
            Self::Modified => "modified",
            Self::Missing => "missing",
        }
    }
}

#[derive(Debug)]
struct StatusResult {
    target: SkillTarget,
    status: SkillInstallStatus,
    metadata: Option<SkillMetadata>,
    installed_hash: Option<String>,
}

impl StatusResult {
    fn to_json(&self) -> Value {
        json!({
            "agent": self.target.agent.id(),
            "agent_display_name": self.target.agent.display_name(),
            "scope": self.target.scope.as_str(),
            "status": self.status.as_str(),
            "path": self.target.skill_dir,
            "installed_hash": self.installed_hash,
            "bundled_hash": bundled_hash(),
            "metadata": self.metadata.as_ref().map(|metadata| json!({
                "schema_version": metadata.schema_version,
                "skill_name": metadata.skill_name,
                "skill_hash": metadata.skill_hash,
                "ctx_cli_version": metadata.ctx_cli_version,
            })),
        })
    }
}

#[derive(Debug)]
struct InstallResult {
    target: SkillTarget,
    success: bool,
    previous_status: SkillInstallStatus,
    status: SkillInstallStatus,
    already_installed: bool,
    updated: bool,
    error: Option<String>,
}

impl InstallResult {
    fn to_json(&self) -> Value {
        json!({
            "agent": self.target.agent.id(),
            "agent_display_name": self.target.agent.display_name(),
            "scope": self.target.scope.as_str(),
            "path": self.target.skill_dir,
            "success": self.success,
            "previous_status": self.previous_status.as_str(),
            "status": self.status.as_str(),
            "already_installed": self.already_installed,
            "updated": self.updated,
            "error": self.error,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SkillMetadata {
    schema_version: u32,
    installer: String,
    skill_name: String,
    skill_hash: String,
    ctx_cli_version: String,
    installed_at: String,
}

impl SkillMetadata {
    fn current() -> Self {
        Self {
            schema_version: 1,
            installer: "ctx-cli".to_owned(),
            skill_name: BUNDLED_SKILL_NAME.to_owned(),
            skill_hash: bundled_hash(),
            ctx_cli_version: env!("CARGO_PKG_VERSION").to_owned(),
            installed_at: utc_now().to_rfc3339(),
        }
    }
}

fn install_target(target: &SkillTarget, force: bool) -> Result<InstallResult> {
    let previous = status_target(target)?;
    if previous.status == SkillInstallStatus::Current {
        if !metadata_is_current(previous.metadata.as_ref()) {
            write_metadata(target)?;
        }
        return Ok(InstallResult {
            target: target.clone(),
            success: true,
            previous_status: previous.status,
            status: SkillInstallStatus::Current,
            already_installed: true,
            updated: false,
            error: None,
        });
    }
    if previous.status == SkillInstallStatus::Modified && !force {
        return Ok(InstallResult {
            target: target.clone(),
            success: false,
            previous_status: previous.status,
            status: previous.status,
            already_installed: false,
            updated: false,
            error: Some("local skill edits detected; rerun with --force to overwrite".to_owned()),
        });
    }
    write_skill_dir(target)?;
    Ok(InstallResult {
        target: target.clone(),
        success: true,
        previous_status: previous.status,
        status: SkillInstallStatus::Current,
        already_installed: false,
        updated: matches!(
            previous.status,
            SkillInstallStatus::Stale | SkillInstallStatus::Modified
        ),
        error: None,
    })
}

fn status_target(target: &SkillTarget) -> Result<StatusResult> {
    ensure_path_inside(&target.base_dir, &target.skill_dir)?;
    let skill_file = target.skill_dir.join("SKILL.md");
    let metadata = read_metadata(&target.skill_dir);
    let installed_hash = match fs::read(&skill_file) {
        Ok(body) => Some(sha256_hex(&body)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => return Err(err).with_context(|| format!("read {}", skill_file.display())),
    };
    let status = match installed_hash.as_deref() {
        None => SkillInstallStatus::Missing,
        Some(hash) if hash == bundled_hash() => SkillInstallStatus::Current,
        Some(hash) => match metadata.as_ref() {
            Some(metadata) if metadata.skill_hash == hash => SkillInstallStatus::Stale,
            _ => SkillInstallStatus::Modified,
        },
    };
    Ok(StatusResult {
        target: target.clone(),
        status,
        metadata,
        installed_hash,
    })
}

fn read_metadata(skill_dir: &Path) -> Option<SkillMetadata> {
    let path = skill_dir.join(METADATA_FILE);
    let body = fs::read(path).ok()?;
    serde_json::from_slice(&body).ok()
}

fn metadata_is_current(metadata: Option<&SkillMetadata>) -> bool {
    metadata.is_some_and(|metadata| {
        metadata.schema_version == 1
            && metadata.installer == "ctx-cli"
            && metadata.skill_name == BUNDLED_SKILL_NAME
            && metadata.skill_hash == bundled_hash()
    })
}

fn write_skill_dir(target: &SkillTarget) -> Result<()> {
    ensure_path_inside(&target.base_dir, &target.skill_dir)?;
    remove_existing_target(&target.skill_dir)
        .with_context(|| format!("remove existing {}", target.skill_dir.display()))?;
    fs::create_dir_all(&target.skill_dir)
        .with_context(|| format!("create {}", target.skill_dir.display()))?;
    fs::write(target.skill_dir.join("SKILL.md"), BUNDLED_SKILL_BODY)
        .with_context(|| format!("write {}", target.skill_dir.join("SKILL.md").display()))?;
    write_metadata(target)
}

fn write_metadata(target: &SkillTarget) -> Result<()> {
    fs::create_dir_all(&target.skill_dir)
        .with_context(|| format!("create {}", target.skill_dir.display()))?;
    let metadata = serde_json::to_vec_pretty(&SkillMetadata::current())?;
    fs::write(target.skill_dir.join(METADATA_FILE), metadata)
        .with_context(|| format!("write {}", target.skill_dir.join(METADATA_FILE).display()))
}

fn remove_existing_target(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || metadata.is_file() => {
            fs::remove_file(path)?;
        }
        Ok(metadata) if metadata.is_dir() => {
            fs::remove_dir_all(path)?;
        }
        Ok(_) => {
            fs::remove_file(path)?;
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(err.into()),
    }
    Ok(())
}

fn print_install_results(results: &[InstallResult]) {
    let all_success = !results.is_empty() && results.iter().all(|result| result.success);
    let all_current = all_success && results.iter().all(|result| result.already_installed);
    let any_updated = results
        .iter()
        .any(|result| result.success && result.updated);
    let any_installed = results
        .iter()
        .any(|result| result.success && !result.already_installed && !result.updated);
    let heading = if all_current {
        "Agent skill already installed"
    } else if all_success && any_updated && !any_installed {
        "Agent skill updated"
    } else if all_success {
        "Agent skill installed"
    } else {
        "Agent skill"
    };
    println!("{heading}: {BUNDLED_SKILL_NAME}");
    for result in results {
        let verb = if result.already_installed {
            "current"
        } else if !result.success {
            "skipped"
        } else if result.updated {
            "updated"
        } else {
            "installed"
        };
        let detail = result
            .error
            .as_deref()
            .map(|error| format!(" - {error}"))
            .unwrap_or_default();
        println!("  {verb}: {}{}", result.target.agent.display_name(), detail);
    }
}

fn print_status_results(results: &[StatusResult]) {
    println!("ctx skill status: {BUNDLED_SKILL_NAME}");
    for result in results {
        println!(
            "  {}: {} ({}) -> {}",
            result.status.as_str(),
            result.target.agent.display_name(),
            result.target.scope.as_str(),
            result.target.skill_dir.display()
        );
    }
}

fn sanitize_skill_name(name: &str) -> Result<String> {
    let mut sanitized = String::with_capacity(name.len());
    let mut previous_dash = false;
    for ch in name.trim().chars().flat_map(char::to_lowercase) {
        let allowed = ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '.' || ch == '_';
        if allowed {
            sanitized.push(ch);
            previous_dash = false;
        } else if !previous_dash {
            sanitized.push('-');
            previous_dash = true;
        }
    }
    let sanitized = sanitized
        .trim_matches(|ch| ch == '.' || ch == '-')
        .chars()
        .take(255)
        .collect::<String>();
    if sanitized.is_empty() || sanitized == "." || sanitized == ".." {
        return Err(anyhow!("invalid skill name"));
    }
    Ok(sanitized)
}

fn ensure_path_inside(base: &Path, target: &Path) -> Result<()> {
    if has_parent_component(base) || has_parent_component(target) {
        return Err(anyhow!("skill path contains parent traversal"));
    }
    if !target.starts_with(base) {
        return Err(anyhow!("skill path escapes target directory"));
    }
    Ok(())
}

fn has_parent_component(path: &Path) -> bool {
    path.components()
        .any(|component| matches!(component, Component::ParentDir))
}

fn bundled_hash() -> String {
    sha256_hex(BUNDLED_SKILL_BODY.as_bytes())
}

fn sha256_hex(body: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(body);
    format!("sha256:{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_target_is_global_canonical_agents_dir() {
        let context = PathContext::for_tests(PathBuf::from("/home/tester"), PathBuf::from("/repo"));
        let targets = resolve_targets(&[], false, false, &context).unwrap();
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].agent, SkillAgentArg::Universal);
        assert_eq!(
            targets[0].skill_dir,
            PathBuf::from("/home/tester/.agents/skills/ctx-agent-history-search")
        );
    }

    #[test]
    fn agent_global_paths_preserve_env_and_xdg_rules() {
        let context = PathContext::for_tests(PathBuf::from("/home/tester"), PathBuf::from("/repo"))
            .with_xdg_config_home(PathBuf::from("/xdg"))
            .with_env_override("CODEX_HOME", PathBuf::from("/codex-home"))
            .with_env_override("CLAUDE_CONFIG_DIR", PathBuf::from("/claude-home"));
        let targets = resolve_targets(
            &[
                SkillAgentArg::Codex,
                SkillAgentArg::ClaudeCode,
                SkillAgentArg::OpenCode,
                SkillAgentArg::XdgAgents,
            ],
            false,
            false,
            &context,
        )
        .unwrap();
        let paths = targets
            .iter()
            .map(|target| (target.agent.id(), target.skill_dir.clone()))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(
            paths["codex"],
            PathBuf::from("/codex-home/skills/ctx-agent-history-search")
        );
        assert_eq!(
            paths["claude-code"],
            PathBuf::from("/claude-home/skills/ctx-agent-history-search")
        );
        assert_eq!(
            paths["opencode"],
            PathBuf::from("/xdg/opencode/skills/ctx-agent-history-search")
        );
        assert_eq!(
            paths["xdg-agents"],
            PathBuf::from("/xdg/agents/skills/ctx-agent-history-search")
        );
    }

    #[test]
    fn project_paths_are_agent_specific_and_relative_to_cwd() {
        let context = PathContext::for_tests(PathBuf::from("/home/tester"), PathBuf::from("/repo"));
        let targets = resolve_targets(
            &[SkillAgentArg::ClaudeCode, SkillAgentArg::Codex],
            false,
            true,
            &context,
        )
        .unwrap();
        let paths = targets
            .iter()
            .map(|target| (target.agent.id(), target.skill_dir.clone()))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(
            paths["claude-code"],
            PathBuf::from("/repo/.claude/skills/ctx-agent-history-search")
        );
        assert_eq!(
            paths["codex"],
            PathBuf::from("/repo/.agents/skills/ctx-agent-history-search")
        );
    }

    #[test]
    fn default_selection_includes_universal_and_detected_agent_specific_dirs() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        fs::create_dir_all(home.join(".claude")).unwrap();
        fs::create_dir_all(home.join(".codex")).unwrap();
        let context = PathContext::for_tests(home, temp.path().join("repo"));

        assert_eq!(
            detected_agents(&context),
            vec![SkillAgentArg::ClaudeCode, SkillAgentArg::Codex]
        );

        let selection = install_agent_selection(
            &SkillInstallArgs {
                agent: Vec::new(),
                all_agents: false,
                project: false,
                json: true,
                force: false,
            },
            &context,
        )
        .unwrap();
        assert_eq!(selection.source, SkillSelectionSource::Detected);
        assert_eq!(
            selection.agents,
            vec![SkillAgentArg::Universal, SkillAgentArg::ClaudeCode]
        );
    }

    #[test]
    fn picker_defaults_to_universal_when_nothing_detected() {
        let temp = tempfile::tempdir().unwrap();
        let context = PathContext::for_tests(temp.path().join("home"), temp.path().join("repo"))
            .with_env_override("CODEX_HOME", temp.path().join("missing-codex"));
        assert_eq!(
            default_picker_agents(&context),
            vec![SkillAgentArg::Universal]
        );
        assert_eq!(
            default_noninteractive_agents(&context),
            (
                vec![SkillAgentArg::Universal],
                SkillSelectionSource::Fallback
            )
        );
    }

    #[test]
    fn picker_selection_accepts_numbers_names_and_all() {
        let options = picker_agents();
        assert_eq!(
            parse_picker_selection("1,2 claude", options).unwrap(),
            vec![SkillAgentArg::Universal, SkillAgentArg::ClaudeCode]
        );
        assert_eq!(
            parse_picker_selection("cursor universal", options).unwrap(),
            vec![SkillAgentArg::Cursor, SkillAgentArg::Universal]
        );
        assert_eq!(parse_picker_selection("all", options).unwrap(), options);
        assert!(parse_picker_selection("99", options).is_err());
        assert!(parse_picker_selection("not-an-agent", options).is_err());
    }

    #[test]
    fn sanitize_blocks_path_traversal_shapes() {
        assert_eq!(
            sanitize_skill_name("../Ctx Agent History Search!!").unwrap(),
            "ctx-agent-history-search"
        );
        assert!(sanitize_skill_name("..").is_err());
        assert!(ensure_path_inside(Path::new("/base"), Path::new("/base/../evil")).is_err());
    }

    #[test]
    fn status_distinguishes_current_stale_modified_and_missing() {
        let temp = tempfile::tempdir().unwrap();
        let context = PathContext::for_tests(temp.path().join("home"), temp.path().join("repo"));
        let target = resolve_targets(&[], false, false, &context)
            .unwrap()
            .remove(0);

        assert_eq!(
            status_target(&target).unwrap().status,
            SkillInstallStatus::Missing
        );

        write_skill_dir(&target).unwrap();
        assert_eq!(
            status_target(&target).unwrap().status,
            SkillInstallStatus::Current
        );

        fs::write(target.skill_dir.join("SKILL.md"), "old bundled content\n").unwrap();
        let old_hash = sha256_hex(b"old bundled content\n");
        let mut metadata = SkillMetadata::current();
        metadata.skill_hash = old_hash;
        fs::write(
            target.skill_dir.join(METADATA_FILE),
            serde_json::to_vec_pretty(&metadata).unwrap(),
        )
        .unwrap();
        assert_eq!(
            status_target(&target).unwrap().status,
            SkillInstallStatus::Stale
        );

        fs::write(target.skill_dir.join("SKILL.md"), "local edits\n").unwrap();
        assert_eq!(
            status_target(&target).unwrap().status,
            SkillInstallStatus::Modified
        );
    }

    #[test]
    fn analytics_properties_are_coarse_and_path_free() {
        let args = SkillArgs {
            command: SkillCommand::Install(SkillInstallArgs {
                agent: vec![SkillAgentArg::Codex, SkillAgentArg::ClaudeCode],
                all_agents: false,
                project: true,
                json: true,
                force: false,
            }),
        };
        let mut properties = analytics::empty_properties();
        args.add_initial_analytics(&mut properties);

        assert_eq!(properties["skill_action"], "install");
        assert_eq!(properties["skill_name"], BUNDLED_SKILL_NAME);
        assert_eq!(properties["skill_scope"], "project");
        assert_eq!(properties["target_agent_group"], "explicit");
        for key in properties.keys() {
            assert!(
                !key.contains("path") && !key.contains("home") && !key.contains("dir"),
                "unexpected path-like analytics key: {key}"
            );
        }
    }
}
