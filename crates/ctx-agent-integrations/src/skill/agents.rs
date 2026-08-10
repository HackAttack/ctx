use std::path::PathBuf;

use super::paths::PathContext;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SkillAgentArg {
    Universal,
    Codex,
    ClaudeCode,
    Cursor,
    OpenCode,
    MiMoCode,
    Amp,
    GeminiCli,
    Antigravity,
    AntigravityCli,
    GitHubCopilot,
    Pi,
    Goose,
}

impl SkillAgentArg {
    pub const ALL: &'static [Self] = &[
        Self::Universal,
        Self::Codex,
        Self::ClaudeCode,
        Self::Cursor,
        Self::OpenCode,
        Self::MiMoCode,
        Self::Amp,
        Self::GeminiCli,
        Self::Antigravity,
        Self::AntigravityCli,
        Self::GitHubCopilot,
        Self::Pi,
        Self::Goose,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Self::Universal => "universal",
            Self::Codex => "codex",
            Self::ClaudeCode => "claude-code",
            Self::Cursor => "cursor",
            Self::OpenCode => "opencode",
            Self::MiMoCode => "mimocode",
            Self::Amp => "amp",
            Self::GeminiCli => "gemini-cli",
            Self::Antigravity => "antigravity",
            Self::AntigravityCli => "antigravity-cli",
            Self::GitHubCopilot => "github-copilot",
            Self::Pi => "pi",
            Self::Goose => "goose",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Universal => "Universal .agents",
            Self::Codex => "Codex",
            Self::ClaudeCode => "Claude Code",
            Self::Cursor => "Cursor",
            Self::OpenCode => "OpenCode",
            Self::MiMoCode => "MiMo Code",
            Self::Amp => "Amp",
            Self::GeminiCli => "Gemini CLI",
            Self::Antigravity => "Antigravity",
            Self::AntigravityCli => "Antigravity CLI",
            Self::GitHubCopilot => "GitHub Copilot",
            Self::Pi => "Pi",
            Self::Goose => "Goose",
        }
    }

    pub fn project_skills_dir(self) -> &'static str {
        match self {
            Self::ClaudeCode => ".claude/skills",
            Self::Pi => ".pi/skills",
            Self::Goose => ".goose/skills",
            Self::Universal
            | Self::Codex
            | Self::Cursor
            | Self::OpenCode
            | Self::MiMoCode
            | Self::Amp
            | Self::GeminiCli
            | Self::Antigravity
            | Self::AntigravityCli
            | Self::GitHubCopilot => ".agents/skills",
        }
    }

    pub fn global_skills_dir(self, context: &PathContext) -> PathBuf {
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
            Self::MiMoCode => context.mimocode_config_dir().join("skills"),
            Self::Amp => context.xdg_config_home.join("agents").join("skills"),
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

    pub fn needs_agent_specific_default(self) -> bool {
        self.project_skills_dir() != ".agents/skills"
    }

    pub fn detect_dir(self, context: &PathContext) -> Option<PathBuf> {
        match self {
            Self::Universal => Some(context.home.join(".agents")),
            Self::Codex => Some(context.env_or_home_child("CODEX_HOME", ".codex")),
            Self::ClaudeCode => Some(context.env_or_home_child("CLAUDE_CONFIG_DIR", ".claude")),
            Self::Cursor => Some(context.home.join(".cursor")),
            Self::OpenCode => Some(context.xdg_config_home.join("opencode")),
            Self::MiMoCode => Some(context.mimocode_config_dir()),
            Self::Amp => Some(context.xdg_config_home.join("amp")),
            Self::GeminiCli => Some(context.home.join(".gemini")),
            Self::Antigravity => Some(context.home.join(".gemini").join("antigravity")),
            Self::AntigravityCli => Some(context.home.join(".gemini").join("antigravity-cli")),
            Self::GitHubCopilot => Some(context.home.join(".copilot")),
            Self::Pi => Some(context.home.join(".pi").join("agent")),
            Self::Goose => Some(context.xdg_config_home.join("goose")),
        }
    }
}

pub fn picker_agents() -> &'static [SkillAgentArg] {
    &[
        SkillAgentArg::Universal,
        SkillAgentArg::ClaudeCode,
        SkillAgentArg::Codex,
        SkillAgentArg::Cursor,
        SkillAgentArg::OpenCode,
        SkillAgentArg::MiMoCode,
        SkillAgentArg::GeminiCli,
        SkillAgentArg::Antigravity,
        SkillAgentArg::AntigravityCli,
        SkillAgentArg::GitHubCopilot,
        SkillAgentArg::Pi,
        SkillAgentArg::Goose,
        SkillAgentArg::Amp,
    ]
}

pub fn agent_from_name(value: &str) -> Option<SkillAgentArg> {
    match value.to_ascii_lowercase().as_str() {
        "universal" | "agents" | ".agents" => Some(SkillAgentArg::Universal),
        "codex" => Some(SkillAgentArg::Codex),
        "claude" | "claude-code" | "claudecode" => Some(SkillAgentArg::ClaudeCode),
        "cursor" => Some(SkillAgentArg::Cursor),
        "opencode" | "open-code" => Some(SkillAgentArg::OpenCode),
        "mimocode" | "mimo-code" | "mimo_code" => Some(SkillAgentArg::MiMoCode),
        "amp" => Some(SkillAgentArg::Amp),
        "gemini" | "gemini-cli" => Some(SkillAgentArg::GeminiCli),
        "antigravity" => Some(SkillAgentArg::Antigravity),
        "antigravity-cli" => Some(SkillAgentArg::AntigravityCli),
        "github-copilot" | "copilot" => Some(SkillAgentArg::GitHubCopilot),
        "pi" => Some(SkillAgentArg::Pi),
        "goose" => Some(SkillAgentArg::Goose),
        _ => None,
    }
}

pub fn parse_skill_agent(value: &str) -> Result<SkillAgentArg, String> {
    agent_from_name(value).ok_or_else(|| format!("unknown skill agent: {value}"))
}
