use std::{
    collections::BTreeMap,
    env,
    ffi::OsString,
    path::{Path, PathBuf},
};

use super::format::{ConfigKind, JsonRoot, JsonServerShape};
use anyhow::{anyhow, Context, Result};

#[derive(Debug, Clone)]
pub struct McpPathContext {
    home: PathBuf,
    xdg_config_home: PathBuf,
    cwd: PathBuf,
    env_overrides: BTreeMap<String, PathBuf>,
}

impl McpPathContext {
    pub fn from_env() -> Result<Self> {
        let home = home_dir().context("resolve home directory")?;
        let xdg_config_home =
            non_empty_env_path("XDG_CONFIG_HOME").unwrap_or_else(|| home.join(".config"));
        let mut env_overrides = BTreeMap::new();
        for key in ["CODEX_HOME", "CLAUDE_CONFIG_DIR", "COPILOT_HOME"] {
            if let Some(path) = non_empty_env_path(key) {
                env_overrides.insert(key.to_owned(), path);
            }
        }
        if let Some(path) = non_empty_absolute_env_path("MIMOCODE_HOME")? {
            env_overrides.insert("MIMOCODE_HOME".to_owned(), path);
        }
        if let Some(path) = absolute_env_path_if_present("GROK_HOME")? {
            env_overrides.insert("GROK_HOME".to_owned(), path);
        }
        if let Some(path) = non_empty_env_path("MIMOCODE_CONFIG_DIR") {
            env_overrides.insert("MIMOCODE_CONFIG_DIR".to_owned(), path);
        }
        Ok(Self {
            home,
            xdg_config_home,
            cwd: env::current_dir().context("resolve current directory")?,
            env_overrides,
        })
    }

    pub fn for_tests(home: PathBuf, cwd: PathBuf) -> Self {
        Self {
            xdg_config_home: home.join(".config"),
            home,
            cwd,
            env_overrides: BTreeMap::new(),
        }
    }

    #[cfg(test)]
    fn with_xdg_config_home(mut self, value: PathBuf) -> Self {
        self.xdg_config_home = value;
        self
    }

    #[cfg(test)]
    fn with_env_override(mut self, key: &str, value: PathBuf) -> Self {
        self.env_overrides.insert(key.to_owned(), value);
        self
    }

    fn env_or_home_child(&self, key: &str, fallback_child: &str) -> PathBuf {
        self.env_overrides
            .get(key)
            .cloned()
            .unwrap_or_else(|| self.home.join(fallback_child))
    }

    fn mimocode_config_dir(&self) -> PathBuf {
        if let Some(path) = self.env_overrides.get("MIMOCODE_CONFIG_DIR") {
            return path.clone();
        }
        self.env_overrides
            .get("MIMOCODE_HOME")
            .map(|home| home.join("config"))
            .unwrap_or_else(|| self.xdg_config_home.join("mimocode"))
    }

    fn mimocode_global_config_file(&self) -> PathBuf {
        existing_or_default(
            [
                self.mimocode_config_dir().join("mimocode.jsonc"),
                self.mimocode_config_dir().join("mimocode.json"),
                self.mimocode_config_dir().join("config.json"),
            ],
            self.mimocode_config_dir().join("mimocode.jsonc"),
        )
    }

    fn mimocode_project_config_file(&self) -> PathBuf {
        existing_or_default(
            [
                self.cwd.join(".mimocode").join("mimocode.jsonc"),
                self.cwd.join(".mimocode").join("mimocode.json"),
                self.cwd.join("mimocode.jsonc"),
                self.cwd.join("mimocode.json"),
            ],
            self.cwd.join(".mimocode").join("mimocode.jsonc"),
        )
    }

    fn claude_user_config(&self) -> PathBuf {
        self.env_overrides
            .get("CLAUDE_CONFIG_DIR")
            .map(|dir| dir.join(".claude.json"))
            .unwrap_or_else(|| self.home.join(".claude.json"))
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

fn absolute_env_path_if_present(key: &str) -> Result<Option<PathBuf>> {
    validate_absolute_env_path(key, env::var_os(key))
}

fn validate_absolute_env_path(key: &str, value: Option<OsString>) -> Result<Option<PathBuf>> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_empty() {
        return Err(anyhow!("{key} must be nonempty and absolute"));
    }
    let path = PathBuf::from(value);
    if !path.is_absolute() {
        return Err(anyhow!(
            "{key} must be an absolute path: {}",
            path.display()
        ));
    }
    Ok(Some(path))
}

fn existing_or_default(paths: impl IntoIterator<Item = PathBuf>, default: PathBuf) -> PathBuf {
    paths
        .into_iter()
        .find(|path| path.is_file())
        .unwrap_or(default)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpAgentArg {
    Codex,
    GrokBuild,
    ClaudeCode,
    Cursor,
    OpenCode,
    MiMoCode,
    GeminiCli,
    QwenCode,
    Goose,
    Kiro,
    Warp,
    Continue,
    Cline,
    GitHubCopilot,
    Zed,
    Windsurf,
    RooCode,
}

pub fn parse_mcp_agent(value: &str) -> std::result::Result<McpAgentArg, String> {
    match value {
        "codex" => Ok(McpAgentArg::Codex),
        "grok-build" | "grok" => Ok(McpAgentArg::GrokBuild),
        "claude-code" | "claude" => Ok(McpAgentArg::ClaudeCode),
        "cursor" => Ok(McpAgentArg::Cursor),
        "opencode" | "open-code" => Ok(McpAgentArg::OpenCode),
        "mimocode" | "mimo-code" | "mimo_code" => Ok(McpAgentArg::MiMoCode),
        "gemini-cli" | "gemini" => Ok(McpAgentArg::GeminiCli),
        "qwen-code" | "qwen" => Ok(McpAgentArg::QwenCode),
        "goose" => Ok(McpAgentArg::Goose),
        "kiro" => Ok(McpAgentArg::Kiro),
        "warp" => Ok(McpAgentArg::Warp),
        "continue" => Ok(McpAgentArg::Continue),
        "cline" => Ok(McpAgentArg::Cline),
        "github-copilot" | "copilot" | "copilot-cli" => Ok(McpAgentArg::GitHubCopilot),
        "zed" => Ok(McpAgentArg::Zed),
        "windsurf" => Ok(McpAgentArg::Windsurf),
        "roo-code" | "roo" => Ok(McpAgentArg::RooCode),
        _ => Err(format!("unknown MCP agent: {value}")),
    }
}

impl McpAgentArg {
    pub const ALL: &'static [Self] = &[
        Self::Codex,
        Self::GrokBuild,
        Self::ClaudeCode,
        Self::Cursor,
        Self::OpenCode,
        Self::MiMoCode,
        Self::GeminiCli,
        Self::QwenCode,
        Self::Goose,
        Self::Kiro,
        Self::Warp,
        Self::Continue,
        Self::Cline,
        Self::GitHubCopilot,
        Self::Zed,
        Self::Windsurf,
    ];
    pub const PROJECT_CAPABLE: &'static [Self] = &[
        Self::Codex,
        Self::GrokBuild,
        Self::ClaudeCode,
        Self::Cursor,
        Self::OpenCode,
        Self::MiMoCode,
        Self::GeminiCli,
        Self::QwenCode,
        Self::Kiro,
        Self::Warp,
        Self::Continue,
        Self::Zed,
        Self::RooCode,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::GrokBuild => "grok-build",
            Self::ClaudeCode => "claude-code",
            Self::Cursor => "cursor",
            Self::OpenCode => "opencode",
            Self::MiMoCode => "mimocode",
            Self::GeminiCli => "gemini-cli",
            Self::QwenCode => "qwen-code",
            Self::Goose => "goose",
            Self::Kiro => "kiro",
            Self::Warp => "warp",
            Self::Continue => "continue",
            Self::Cline => "cline",
            Self::GitHubCopilot => "github-copilot",
            Self::Zed => "zed",
            Self::Windsurf => "windsurf",
            Self::RooCode => "roo-code",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::GrokBuild => "Grok Build",
            Self::ClaudeCode => "Claude Code",
            Self::Cursor => "Cursor",
            Self::OpenCode => "OpenCode",
            Self::MiMoCode => "MiMo Code",
            Self::GeminiCli => "Gemini CLI",
            Self::QwenCode => "Qwen Code",
            Self::Goose => "Goose",
            Self::Kiro => "Kiro",
            Self::Warp => "Warp",
            Self::Continue => "Continue",
            Self::Cline => "Cline",
            Self::GitHubCopilot => "GitHub Copilot CLI",
            Self::Zed => "Zed",
            Self::Windsurf => "Windsurf",
            Self::RooCode => "Roo Code",
        }
    }

    pub fn detected(self, context: &McpPathContext) -> bool {
        match self {
            Self::Codex => {
                context.env_overrides.contains_key("CODEX_HOME")
                    || context.home.join(".codex").exists()
                    || Path::new("/etc/codex").exists()
            }
            Self::GrokBuild => {
                context.env_overrides.contains_key("GROK_HOME")
                    || context.home.join(".grok").exists()
            }
            Self::ClaudeCode => {
                context.env_overrides.contains_key("CLAUDE_CONFIG_DIR")
                    || context.home.join(".claude").exists()
                    || context.home.join(".claude.json").exists()
            }
            Self::Cursor => context.home.join(".cursor").exists(),
            Self::OpenCode => context.xdg_config_home.join("opencode").exists(),
            Self::MiMoCode => {
                context.env_overrides.contains_key("MIMOCODE_HOME")
                    || context.env_overrides.contains_key("MIMOCODE_CONFIG_DIR")
                    || context.mimocode_config_dir().exists()
            }
            Self::GeminiCli => context.home.join(".gemini").exists(),
            Self::QwenCode => context.home.join(".qwen").exists(),
            Self::Goose => context.xdg_config_home.join("goose").exists(),
            Self::Kiro => context.home.join(".kiro").exists(),
            Self::Warp => context.home.join(".warp").exists(),
            Self::Continue => context.home.join(".continue").join("config.yaml").exists(),
            Self::Cline => context.home.join(".cline").exists(),
            Self::GitHubCopilot => {
                context.env_overrides.contains_key("COPILOT_HOME")
                    || context.home.join(".copilot").exists()
            }
            Self::Zed => context.xdg_config_home.join("zed").exists(),
            Self::Windsurf => context.home.join(".codeium").exists(),
            Self::RooCode => {
                context.home.join(".roo").exists() || context.cwd.join(".roo").exists()
            }
        }
    }

    pub fn target(self, project: bool, context: &McpPathContext) -> McpTarget {
        if project {
            return self.project_target(context);
        }
        self.global_target(context)
    }

    fn global_target(self, context: &McpPathContext) -> McpTarget {
        let (path, kind) = match self {
            Self::Codex => (
                context
                    .env_or_home_child("CODEX_HOME", ".codex")
                    .join("config.toml"),
                ConfigKind::CodexToml,
            ),
            Self::GrokBuild => (
                context
                    .env_or_home_child("GROK_HOME", ".grok")
                    .join("config.toml"),
                ConfigKind::CodexToml,
            ),
            Self::ClaudeCode => (
                context.claude_user_config(),
                ConfigKind::Json {
                    root: JsonRoot::McpServers,
                    server: JsonServerShape::StdioType,
                },
            ),
            Self::Cursor => (
                context.home.join(".cursor").join("mcp.json"),
                ConfigKind::Json {
                    root: JsonRoot::McpServers,
                    server: JsonServerShape::StdioType,
                },
            ),
            Self::OpenCode => (
                context
                    .xdg_config_home
                    .join("opencode")
                    .join("opencode.json"),
                ConfigKind::opencode_json(),
            ),
            Self::MiMoCode => (
                context.mimocode_global_config_file(),
                ConfigKind::opencode_json(),
            ),
            Self::GeminiCli => (
                context.home.join(".gemini").join("settings.json"),
                ConfigKind::Json {
                    root: JsonRoot::McpServers,
                    server: JsonServerShape::Plain,
                },
            ),
            Self::QwenCode => (
                context.home.join(".qwen").join("settings.json"),
                ConfigKind::Json {
                    root: JsonRoot::McpServers,
                    server: JsonServerShape::Plain,
                },
            ),
            Self::Goose => (
                context.xdg_config_home.join("goose").join("config.yaml"),
                ConfigKind::GooseYaml,
            ),
            Self::Kiro => (
                context.home.join(".kiro").join("settings").join("mcp.json"),
                ConfigKind::Json {
                    root: JsonRoot::McpServers,
                    server: JsonServerShape::Plain,
                },
            ),
            Self::Warp => (
                context.home.join(".warp").join(".mcp.json"),
                ConfigKind::Json {
                    root: JsonRoot::McpServers,
                    server: JsonServerShape::Plain,
                },
            ),
            Self::Continue => (
                context.home.join(".continue").join("config.yaml"),
                ConfigKind::ContinueYaml,
            ),
            Self::Cline => (
                context.home.join(".cline").join("mcp.json"),
                ConfigKind::Json {
                    root: JsonRoot::McpServers,
                    server: JsonServerShape::ClineLocal,
                },
            ),
            Self::GitHubCopilot => (
                context
                    .env_or_home_child("COPILOT_HOME", ".copilot")
                    .join("mcp-config.json"),
                ConfigKind::Json {
                    root: JsonRoot::McpServers,
                    server: JsonServerShape::CopilotLocal,
                },
            ),
            Self::Zed => (
                context.xdg_config_home.join("zed").join("settings.json"),
                ConfigKind::Json {
                    root: JsonRoot::ContextServers,
                    server: JsonServerShape::Plain,
                },
            ),
            Self::Windsurf => (
                context.home.join(".codeium").join("mcp_config.json"),
                ConfigKind::Json {
                    root: JsonRoot::McpServers,
                    server: JsonServerShape::Plain,
                },
            ),
            Self::RooCode => {
                return McpTarget::unsupported(
                    self,
                    McpScope::Global,
                    "global Roo Code MCP config path is managed by the extension UI and is not stable across hosts",
                );
            }
        };
        McpTarget::supported(self, McpScope::Global, path, kind, self.detected(context))
    }

    fn project_target(self, context: &McpPathContext) -> McpTarget {
        let target = match self {
            Self::Codex => Some((
                context.cwd.join(".codex").join("config.toml"),
                ConfigKind::CodexToml,
            )),
            Self::GrokBuild => Some((
                context.cwd.join(".grok").join("config.toml"),
                ConfigKind::CodexToml,
            )),
            Self::ClaudeCode => Some((
                context.cwd.join(".mcp.json"),
                ConfigKind::Json {
                    root: JsonRoot::McpServers,
                    server: JsonServerShape::StdioType,
                },
            )),
            Self::Cursor => Some((
                context.cwd.join(".cursor").join("mcp.json"),
                ConfigKind::Json {
                    root: JsonRoot::McpServers,
                    server: JsonServerShape::StdioType,
                },
            )),
            Self::OpenCode => Some((
                context.cwd.join("opencode.json"),
                ConfigKind::opencode_json(),
            )),
            Self::MiMoCode => Some((
                context.mimocode_project_config_file(),
                ConfigKind::opencode_json(),
            )),
            Self::GeminiCli => Some((
                context.cwd.join(".gemini").join("settings.json"),
                ConfigKind::Json {
                    root: JsonRoot::McpServers,
                    server: JsonServerShape::Plain,
                },
            )),
            Self::QwenCode => Some((
                context.cwd.join(".qwen").join("settings.json"),
                ConfigKind::Json {
                    root: JsonRoot::McpServers,
                    server: JsonServerShape::Plain,
                },
            )),
            Self::Kiro => Some((
                context.cwd.join(".kiro").join("settings").join("mcp.json"),
                ConfigKind::Json {
                    root: JsonRoot::McpServers,
                    server: JsonServerShape::Plain,
                },
            )),
            Self::Warp => Some((
                context.cwd.join(".warp").join(".mcp.json"),
                ConfigKind::Json {
                    root: JsonRoot::McpServers,
                    server: JsonServerShape::Plain,
                },
            )),
            Self::Continue => Some((
                context
                    .cwd
                    .join(".continue")
                    .join("mcpServers")
                    .join("ctx.yaml"),
                ConfigKind::ContinueYaml,
            )),
            Self::Zed => Some((
                context.cwd.join(".zed").join("settings.json"),
                ConfigKind::Json {
                    root: JsonRoot::ContextServers,
                    server: JsonServerShape::Plain,
                },
            )),
            Self::RooCode => Some((
                context.cwd.join(".roo").join("mcp.json"),
                ConfigKind::Json {
                    root: JsonRoot::McpServers,
                    server: JsonServerShape::Plain,
                },
            )),
            Self::Cline | Self::Goose | Self::GitHubCopilot | Self::Windsurf => None,
        };
        match target {
            Some((path, kind)) => McpTarget::supported(
                self,
                McpScope::Project,
                path,
                kind,
                project_detection_path(self, context).exists(),
            ),
            None => McpTarget::unsupported(
                self,
                McpScope::Project,
                "project-scoped MCP config is not documented for this agent",
            ),
        }
    }
}

pub fn project_detection_path(agent: McpAgentArg, context: &McpPathContext) -> PathBuf {
    match agent {
        McpAgentArg::Codex => context.cwd.join(".codex"),
        McpAgentArg::GrokBuild => context.cwd.join(".grok"),
        McpAgentArg::ClaudeCode => context.cwd.join(".mcp.json"),
        McpAgentArg::Cursor => context.cwd.join(".cursor"),
        McpAgentArg::OpenCode => context.cwd.join("opencode.json"),
        McpAgentArg::MiMoCode => context.cwd.join(".mimocode"),
        McpAgentArg::GeminiCli => context.cwd.join(".gemini"),
        McpAgentArg::QwenCode => context.cwd.join(".qwen"),
        McpAgentArg::Kiro => context.cwd.join(".kiro"),
        McpAgentArg::Warp => context.cwd.join(".warp"),
        McpAgentArg::Continue => context.cwd.join(".continue"),
        McpAgentArg::Zed => context.cwd.join(".zed"),
        McpAgentArg::RooCode => context.cwd.join(".roo"),
        McpAgentArg::Cline
        | McpAgentArg::Goose
        | McpAgentArg::GitHubCopilot
        | McpAgentArg::Windsurf => context.cwd.clone(),
    }
}

#[derive(Debug, Clone, Copy)]
pub enum McpScope {
    Global,
    Project,
}

impl McpScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Project => "project",
        }
    }
}

#[derive(Debug, Clone)]
pub struct McpTarget {
    pub agent: McpAgentArg,
    pub scope: McpScope,
    pub path: Option<PathBuf>,
    pub kind: Option<ConfigKind>,
    pub detected: bool,
    pub unsupported_reason: Option<String>,
}

impl McpTarget {
    fn supported(
        agent: McpAgentArg,
        scope: McpScope,
        path: PathBuf,
        kind: ConfigKind,
        detected: bool,
    ) -> Self {
        Self {
            agent,
            scope,
            path: Some(path),
            kind: Some(kind),
            detected,
            unsupported_reason: None,
        }
    }

    fn unsupported(agent: McpAgentArg, scope: McpScope, reason: &str) -> Self {
        Self {
            agent,
            scope,
            path: None,
            kind: None,
            detected: false,
            unsupported_reason: Some(reason.to_owned()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn grok_build_parser_uses_canonical_id_and_only_documented_alias() {
        assert_eq!(parse_mcp_agent("grok-build"), Ok(McpAgentArg::GrokBuild));
        assert_eq!(parse_mcp_agent("grok"), Ok(McpAgentArg::GrokBuild));
        assert!(parse_mcp_agent("grokbuild").is_err());
        assert!(parse_mcp_agent("grok_build").is_err());
        assert_eq!(McpAgentArg::GrokBuild.id(), "grok-build");
        assert_eq!(McpAgentArg::GrokBuild.display_name(), "Grok Build");
    }

    #[test]
    fn grok_home_contract_rejects_empty_and_relative_values() {
        assert!(validate_absolute_env_path("GROK_HOME", Some(OsString::new())).is_err());
        assert!(validate_absolute_env_path("GROK_HOME", Some("relative".into())).is_err());
        assert_eq!(
            validate_absolute_env_path("GROK_HOME", Some("/grok-home".into())).unwrap(),
            Some(PathBuf::from("/grok-home"))
        );
        assert_eq!(validate_absolute_env_path("GROK_HOME", None).unwrap(), None);
    }

    #[test]
    fn grok_build_targets_use_native_paths_and_codex_compatible_toml() {
        let context =
            McpPathContext::for_tests(PathBuf::from("/home/tester"), PathBuf::from("/repo"));
        let global = McpAgentArg::GrokBuild.target(false, &context);
        assert_eq!(
            global.path,
            Some(PathBuf::from("/home/tester/.grok/config.toml"))
        );
        assert!(matches!(global.kind, Some(ConfigKind::CodexToml)));

        let project = McpAgentArg::GrokBuild.target(true, &context);
        assert_eq!(project.path, Some(PathBuf::from("/repo/.grok/config.toml")));
        assert!(matches!(project.kind, Some(ConfigKind::CodexToml)));

        let override_context = context.with_env_override("GROK_HOME", PathBuf::from("/grok-home"));
        let overridden = McpAgentArg::GrokBuild.target(false, &override_context);
        assert_eq!(
            overridden.path,
            Some(PathBuf::from("/grok-home/config.toml"))
        );
        assert!(McpAgentArg::GrokBuild.detected(&override_context));
    }

    #[test]
    fn grok_build_install_writes_the_codex_compatible_server_table() {
        let temp = tempfile::tempdir().unwrap();
        let context = McpPathContext::for_tests(temp.path().join("home"), temp.path().join("repo"));
        let target = McpAgentArg::GrokBuild.target(true, &context);

        let result = super::super::install_target(&target, false);
        assert!(result.success, "{:?}", result.error);
        let body = fs::read_to_string(target.path.as_ref().unwrap()).unwrap();
        assert!(body.contains("[mcp_servers.ctx]"));
        assert!(body.contains("command = \"ctx\""));
        assert!(body.contains("args = [\"mcp\", \"serve\"]"));
        assert_eq!(
            super::super::status_target(&target).status,
            super::super::ConfigStatus::Current
        );
    }

    #[test]
    fn detection_uses_home_xdg_and_env_paths() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let xdg = temp.path().join("xdg");
        fs::create_dir_all(home.join(".cursor")).unwrap();
        fs::create_dir_all(xdg.join("opencode")).unwrap();
        fs::create_dir_all(xdg.join("mimocode")).unwrap();
        let context = McpPathContext::for_tests(home, temp.path().join("repo"))
            .with_xdg_config_home(xdg)
            .with_env_override("CODEX_HOME", temp.path().join("codex-home"));
        assert!(McpAgentArg::Codex.detected(&context));
        assert!(McpAgentArg::Cursor.detected(&context));
        assert!(McpAgentArg::OpenCode.detected(&context));
        assert!(McpAgentArg::MiMoCode.detected(&context));
        assert!(!McpAgentArg::QwenCode.detected(&context));
    }

    #[test]
    fn detection_treats_mimocode_config_dir_env_as_present() {
        let temp = tempfile::tempdir().unwrap();
        let context = McpPathContext::for_tests(temp.path().join("home"), temp.path().join("repo"))
            .with_env_override(
                "MIMOCODE_CONFIG_DIR",
                temp.path().join("new-mimocode-config"),
            );

        assert!(McpAgentArg::MiMoCode.detected(&context));
    }

    #[test]
    fn project_target_reports_unsupported_for_global_only_agents() {
        let temp = tempfile::tempdir().unwrap();
        let context = McpPathContext::for_tests(temp.path().join("home"), temp.path().join("repo"));
        let target = McpAgentArg::GitHubCopilot.target(true, &context);
        assert!(target.path.is_none());
        assert!(target.kind.is_none());
        assert_eq!(
            target.unsupported_reason.as_deref(),
            Some("project-scoped MCP config is not documented for this agent")
        );
    }
}
