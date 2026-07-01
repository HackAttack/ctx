use std::{
    collections::BTreeMap,
    env, fs,
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result};

pub const CONFIG_FILE: &str = "config.toml";

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub analytics: AnalyticsConfig,
    pub upgrade: UpgradeConfig,
}

#[derive(Debug, Clone)]
pub struct AnalyticsConfig {
    pub enabled: bool,
    pub endpoint: String,
}

#[derive(Debug, Clone)]
pub struct UpgradeConfig {
    pub auto: String,
    pub channel: String,
    pub interval: Duration,
    pub functions_base: String,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            analytics: AnalyticsConfig {
                enabled: true,
                endpoint: "https://cli.ctx.rs/functions/v1/analytics".to_owned(),
            },
            upgrade: UpgradeConfig {
                auto: "apply".to_owned(),
                channel: "stable".to_owned(),
                interval: Duration::from_secs(24 * 60 * 60),
                functions_base: "https://cli.ctx.rs/functions/v1".to_owned(),
            },
        }
    }
}

impl AppConfig {
    pub fn load(data_root: &Path) -> Result<Self> {
        let mut config = Self::default();
        let path = data_root.join(CONFIG_FILE);
        if path.exists() {
            let text =
                fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
            let parsed = parse_toml_subset(&text);
            config.apply_values(&parsed);
        }
        config.apply_env();
        Ok(config)
    }

    fn apply_values(&mut self, values: &BTreeMap<String, String>) {
        if let Some(enabled) = parse_bool(values.get("analytics.enabled")) {
            self.analytics.enabled = enabled;
        }
        if let Some(endpoint) = parse_string(values.get("analytics.endpoint")) {
            self.analytics.endpoint = endpoint;
        }
        if let Some(auto) = parse_string(values.get("upgrade.auto")) {
            self.upgrade.auto = auto;
        }
        if let Some(channel) = parse_string(values.get("upgrade.channel")) {
            self.upgrade.channel = channel;
        }
        if let Some(hours) = parse_u64(values.get("upgrade.interval_hours")) {
            self.upgrade.interval = Duration::from_secs(hours.saturating_mul(60 * 60));
        }
        if let Some(seconds) = parse_u64(values.get("upgrade.interval_seconds")) {
            self.upgrade.interval = Duration::from_secs(seconds);
        }
        if let Some(functions_base) = parse_string(values.get("upgrade.functions_base")) {
            self.upgrade.functions_base = functions_base;
        }
    }

    fn apply_env(&mut self) {
        if let Ok(value) = env::var("CTX_ANALYTICS_ENABLED") {
            if let Some(enabled) = parse_bool_value(&value) {
                self.analytics.enabled = enabled;
            }
        }
        if env_flag("CTX_ANALYTICS_OFF") || env_flag("CTX_DISABLE_ANALYTICS") {
            self.analytics.enabled = false;
        }
        if let Ok(endpoint) = env::var("CTX_ANALYTICS_ENDPOINT") {
            if !endpoint.trim().is_empty() {
                self.analytics.endpoint = endpoint;
            }
        }
        if env_flag("CTX_UPGRADE_OFF") || env_flag("CTX_DISABLE_AUTO_UPGRADE") {
            self.upgrade.auto = "off".to_owned();
        }
        if let Ok(auto) = env::var("CTX_UPGRADE_AUTO") {
            if !auto.trim().is_empty() {
                self.upgrade.auto = auto;
            }
        }
        if let Ok(channel) = env::var("CTX_CHANNEL").or_else(|_| env::var("CTX_UPGRADE_CHANNEL")) {
            if !channel.trim().is_empty() {
                self.upgrade.channel = channel;
            }
        }
        if let Ok(functions_base) = env::var("CTX_FUNCTIONS_BASE") {
            if !functions_base.trim().is_empty() {
                self.upgrade.functions_base = functions_base;
            }
        }
        if let Ok(seconds) = env::var("CTX_UPGRADE_INTERVAL_SECONDS") {
            if let Ok(seconds) = seconds.parse::<u64>() {
                self.upgrade.interval = Duration::from_secs(seconds);
            }
        }
    }

    pub fn config_path(data_root: &Path) -> PathBuf {
        data_root.join(CONFIG_FILE)
    }
}

pub fn write_default_config(data_root: &Path) -> Result<()> {
    let path = AppConfig::config_path(data_root);
    if path.exists() {
        return Ok(());
    }
    let mut file = fs::File::create(&path)?;
    file.write_all(
        b"[upgrade]\n\
auto = \"apply\"\n\
channel = \"stable\"\n\
interval_hours = 24\n",
    )?;
    Ok(())
}

fn parse_toml_subset(text: &str) -> BTreeMap<String, String> {
    let mut section = String::new();
    let mut values = BTreeMap::new();
    for raw_line in text.lines() {
        let line = raw_line.split('#').next().unwrap_or_default().trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            section = line
                .trim_start_matches('[')
                .trim_end_matches(']')
                .trim()
                .to_owned();
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() {
            continue;
        }
        let full_key = if section.is_empty() {
            key.to_owned()
        } else {
            format!("{section}.{key}")
        };
        values.insert(
            full_key,
            value.trim().trim_end_matches(',').trim().to_owned(),
        );
    }
    values
}

fn parse_string(value: Option<&String>) -> Option<String> {
    value
        .map(|value| value.trim().trim_matches('"').trim_matches('\'').to_owned())
        .filter(|value| !value.is_empty())
}

fn parse_bool(value: Option<&String>) -> Option<bool> {
    value.and_then(|value| parse_bool_value(value))
}

fn parse_u64(value: Option<&String>) -> Option<u64> {
    value.and_then(|value| value.trim().trim_matches('"').parse::<u64>().ok())
}

fn parse_bool_value(value: &str) -> Option<bool> {
    match value.trim().trim_matches('"').to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Some(true),
        "false" | "0" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn env_flag(key: &str) -> bool {
    env::var_os(key).is_some_and(|value| {
        let value = value.to_string_lossy();
        !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "" | "0" | "false" | "no" | "off"
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_day_one_config_values() {
        let values = parse_toml_subset(
            r#"
[analytics]
enabled = false

[upgrade]
auto = "off"
channel = "beta"
interval_seconds = 60
"#,
        );
        let mut config = AppConfig::default();
        assert_eq!(
            config.analytics.endpoint,
            "https://cli.ctx.rs/functions/v1/analytics"
        );
        assert!(config.analytics.enabled);
        assert_eq!(config.upgrade.auto, "apply");
        config.apply_values(&values);
        assert!(!config.analytics.enabled);
        assert_eq!(config.upgrade.auto, "off");
        assert_eq!(config.upgrade.channel, "beta");
        assert_eq!(config.upgrade.interval, Duration::from_secs(60));
    }
}
