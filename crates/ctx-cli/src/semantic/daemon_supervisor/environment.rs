#[cfg(any(test, unix, windows))]
use std::path::PathBuf;
use std::{collections::BTreeMap, env, ffi::OsString, path::Path};

use anyhow::{anyhow, Result};
use ctx_daemon_runtime::{NormalizedLaunch, SupervisorIdentity, SupervisorSpec};
use ctx_history_core::utc_now;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::compact_json;

const SUPERVISOR_DAEMON_POLICY_ENV_ALLOWLIST: &[&str] = &[
    "ALL_PROXY",
    "CURL_CA_BUNDLE",
    "CTX_ANALYTICS_ENABLED",
    "CTX_DAEMON_ENABLED",
    "CTX_DAEMON_MODE",
    "CTX_LOCAL_USAGE_ENABLED",
    "CTX_PRO_CHANNEL",
    "CTX_SEARCH_SEMANTIC",
    "CTX_UPGRADE_AUTO",
    "CTX_UPGRADE_CHANNEL",
    "CTX_UPGRADE_INTERVAL_SECONDS",
    "DBUS_SESSION_BUS_ADDRESS",
    "HOMEDRIVE",
    "HOMEPATH",
    "HOME",
    "HTTPS_PROXY",
    "HTTP_PROXY",
    "LANG",
    "LC_ALL",
    "LOCALAPPDATA",
    "MIMOCODE_CONFIG_DIR",
    "NO_PROXY",
    "REQUESTS_CA_BUNDLE",
    "SSL_CERT_DIR",
    "SSL_CERT_FILE",
    "SystemRoot",
    "TEMP",
    "TMP",
    "TMPDIR",
    "TZ",
    "USERPROFILE",
    "WINDIR",
    "XDG_RUNTIME_DIR",
    "all_proxy",
    "https_proxy",
    "http_proxy",
    "no_proxy",
];
const PRO_CHANNEL_ENV: &str = "CTX_PRO_CHANNEL";
#[cfg(any(test, target_os = "linux"))]
pub(super) const SYSTEMD_UNIT_NAME: &str = "ctx.service";
#[cfg(any(test, target_os = "macos"))]
pub(super) const LAUNCH_AGENT_LABEL: &str = "rs.ctx.daemon";
pub(super) const SUPERVISOR_DESCRIPTION: &str = "ctx persistent history daemon";
const SUPERVISOR_DAEMON_FIXED_PATH: &str = if cfg!(windows) {
    r"C:\Windows\System32;C:\Windows"
} else {
    "/usr/local/bin:/usr/bin:/bin"
};

#[derive(Debug, Clone, Eq, PartialEq)]
pub(super) struct SupervisorEnvironmentSnapshot {
    pub(super) values: Vec<(String, String)>,
    captured_at_ms: i64,
    sha256: String,
}

impl SupervisorEnvironmentSnapshot {
    pub(super) fn contract_report(&self) -> Value {
        compact_json(json!({
            "schema_version": 1,
            "captured_at_ms": self.captured_at_ms,
            "allowlist": supervisor_environment_allowlist_names(),
            "captured_names": self
                .values
                .iter()
                .map(|(name, _)| name)
                .collect::<Vec<_>>(),
            "sha256": self.sha256,
            "values_exposed": false,
            "error": Value::Null,
        }))
    }
}

pub(super) fn supervisor_environment_snapshot() -> Result<SupervisorEnvironmentSnapshot> {
    let mut values = BTreeMap::new();
    for name in ctx_history_capture::provider_sources::DISCOVERY_ENV_ALLOWLIST
        .iter()
        .chain(SUPERVISOR_DAEMON_POLICY_ENV_ALLOWLIST)
    {
        if *name == PRO_CHANNEL_ENV {
            continue;
        }
        let Some(value) = env::var_os(name) else {
            continue;
        };
        values.insert(
            (*name).to_owned(),
            validated_supervisor_environment_value(name, value)?,
        );
    }
    if let Some(channel) = validated_supervisor_pro_channel(env::var_os(PRO_CHANNEL_ENV))? {
        values.insert(PRO_CHANNEL_ENV.to_owned(), channel);
    }
    values.insert("PATH".to_owned(), SUPERVISOR_DAEMON_FIXED_PATH.to_owned());
    #[cfg(unix)]
    if !values.contains_key("HOME") {
        if let Some(home) = crate::identity::home_dir() {
            let home = validated_supervisor_fallback_home(home)?;
            values.insert("HOME".to_owned(), home);
        }
    }

    let values = values.into_iter().collect::<Vec<_>>();
    let mut digest = Sha256::new();
    for (name, value) in &values {
        digest.update(u64::try_from(name.len()).unwrap_or(u64::MAX).to_le_bytes());
        digest.update(name.as_bytes());
        digest.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_le_bytes());
        digest.update(value.as_bytes());
    }
    Ok(SupervisorEnvironmentSnapshot {
        values,
        captured_at_ms: utc_now().timestamp_millis(),
        sha256: format!("{:x}", digest.finalize()),
    })
}

pub(super) fn supervisor_environment_contract_report() -> Value {
    match supervisor_environment_snapshot() {
        Ok(snapshot) => snapshot.contract_report(),
        Err(error) => compact_json(json!({
            "schema_version": 1,
            "captured_at_ms": utc_now().timestamp_millis(),
            "allowlist": supervisor_environment_allowlist_names(),
            "captured_names": [],
            "sha256": Value::Null,
            "values_exposed": false,
            "error": format!("{error:#}"),
        })),
    }
}

#[cfg(test)]
pub(super) fn linux_systemd_unit_with_environment(
    executable: &Path,
    data_root: &Path,
    snapshot: &SupervisorEnvironmentSnapshot,
) -> Result<String> {
    let identity = supervisor_identity(SYSTEMD_UNIT_NAME, PathBuf::from(SYSTEMD_UNIT_NAME))?;
    ctx_daemon_runtime::linux_systemd_unit(&supervisor_artifact_spec(
        identity, executable, data_root, snapshot,
    )?)
}

#[cfg(any(test, target_os = "macos"))]
pub(super) fn launch_agent_plist_with_environment(
    executable: &Path,
    data_root: &Path,
    snapshot: &SupervisorEnvironmentSnapshot,
) -> Result<String> {
    let identity = supervisor_identity(
        LAUNCH_AGENT_LABEL,
        PathBuf::from(format!("{LAUNCH_AGENT_LABEL}.plist")),
    )?;
    ctx_daemon_runtime::launch_agent_plist(&supervisor_artifact_spec(
        identity, executable, data_root, snapshot,
    )?)
}

#[cfg(any(test, target_os = "linux", target_os = "macos", windows))]
pub(super) fn supervisor_identity(
    name: &str,
    artifact_path: PathBuf,
) -> Result<SupervisorIdentity> {
    SupervisorIdentity::new(name, artifact_path)
}

#[cfg(any(test, windows))]
pub(super) fn windows_supervisor_identity(
    data_root: &Path,
    user_sid: &str,
) -> Result<SupervisorIdentity> {
    supervisor_identity(
        &format!(r"\ctx-daemon-{user_sid}"),
        ctx_daemon_runtime::daemon_root_path(data_root).join("windows-task.xml"),
    )
}

pub(super) fn supervisor_artifact_spec(
    identity: SupervisorIdentity,
    executable: &Path,
    data_root: &Path,
    snapshot: &SupervisorEnvironmentSnapshot,
) -> Result<SupervisorSpec> {
    let environment = snapshot
        .values
        .iter()
        .map(|(name, value)| (OsString::from(name), OsString::from(value)))
        .collect();
    SupervisorSpec::new(
        identity,
        SUPERVISOR_DESCRIPTION,
        NormalizedLaunch::new(
            executable.to_path_buf(),
            vec![
                OsString::from("--data-root"),
                data_root.as_os_str().to_os_string(),
                OsString::from("daemon"),
                OsString::from("run"),
                OsString::from("--format=json"),
            ],
            environment,
        ),
    )
}

fn validated_supervisor_environment_value(name: &str, value: OsString) -> Result<String> {
    let value = value.into_string().map_err(|_| {
        anyhow!(
            "supervisor environment variable {name} is not Unicode; remove it or persist the path in ctx configuration"
        )
    })?;
    validated_supervisor_artifact_text(&format!("environment variable {name}"), &value)?;
    Ok(value)
}

fn validated_supervisor_pro_channel(value: Option<OsString>) -> Result<Option<String>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = validated_supervisor_environment_value(PRO_CHANNEL_ENV, value)?;
    if matches!(value.as_str(), "stable" | "staging") {
        return Ok(Some(value));
    }
    Err(anyhow!(
        "supervisor environment variable {PRO_CHANNEL_ENV} must be stable or staging"
    ))
}

#[cfg(unix)]
fn validated_supervisor_fallback_home(home: PathBuf) -> Result<String> {
    validated_supervisor_environment_value("HOME", home.into_os_string())
}

#[cfg(all(test, windows))]
pub(super) fn validated_supervisor_artifact_path<'a>(
    label: &str,
    path: &'a Path,
) -> Result<&'a str> {
    ctx_daemon_runtime::validated_supervisor_artifact_path(label, path)
}

pub(super) fn validated_supervisor_artifact_text<'a>(
    label: &str,
    value: &'a str,
) -> Result<&'a str> {
    ctx_daemon_runtime::validated_supervisor_artifact_text(label, value)
}

fn supervisor_environment_allowlist_names() -> Vec<&'static str> {
    let mut names = ctx_history_capture::provider_sources::DISCOVERY_ENV_ALLOWLIST.to_vec();
    names.extend_from_slice(SUPERVISOR_DAEMON_POLICY_ENV_ALLOWLIST);
    names.push("PATH");
    names.sort_unstable();
    names.dedup();
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contract_is_narrow_nonsecret_and_rejects_controls() {
        let allowlist = supervisor_environment_allowlist_names();
        for required in [
            "CODEX_HOME",
            "CLAUDE_CONFIG_DIR",
            "COPILOT_HOME",
            "XDG_CONFIG_HOME",
            "CTX_LOCAL_USAGE_ENABLED",
            "CTX_ANALYTICS_ENABLED",
            "CTX_PRO_CHANNEL",
            "CTX_UPGRADE_AUTO",
            "CTX_UPGRADE_CHANNEL",
            "CTX_UPGRADE_INTERVAL_SECONDS",
            "HTTPS_PROXY",
            "MIMOCODE_CONFIG_DIR",
            "NO_PROXY",
            "SSL_CERT_FILE",
            "CURL_CA_BUNDLE",
        ] {
            assert!(allowlist.contains(&required), "missing {required}");
        }
        for forbidden in [
            "CTX_PRO_HELPER",
            "CTX_SEMANTIC_MODEL_ONNX",
            "CTX_SEMANTIC_COREML_NATIVE_COMPUTE",
            "CTX_ANALYTICS_ENDPOINT",
            "CTX_RELEASE_INHERITED_AUTHORITY",
            "CTX_RELEASE_CONFIGURED_AUTHORITY",
            "CTX_RELEASE_BASE_URL",
            "CTX_RELEASE_METADATA_URL",
            "CTX_RELEASE_METADATA_PUBLIC_KEY_PEM",
            "CTX_RELEASE_METADATA_SIGNATURE_URL",
            "CTX_RELEASE_PUBLIC_KEY",
            "CTX_RELEASE_SIGNATURE",
            "CTX_RELEASE_SELF_UPGRADE_ALLOWED",
            "CTX_RELEASE_VERSION",
            "CTX_PRO_STAGING_ACCESS_CLIENT_ID",
            "CTX_PRO_STAGING_ACCESS_CLIENT_SECRET",
            "CTX_PRO_QUALIFICATION_HELPER_PATH",
            "CTX_PRO_QUALIFICATION_HELPER_SHA256",
            "CTX_PRO_QUALIFICATION_HELPER_CHANNEL",
            "AWS_SECRET_ACCESS_KEY",
            "GITHUB_TOKEN",
        ] {
            assert!(!allowlist.contains(&forbidden), "captured {forbidden}");
        }
        for hostile in [
            "line\nbreak",
            "carriage\rreturn",
            "tab\tvalue",
            "nul\0value",
        ] {
            let error =
                validated_supervisor_environment_value("CODEX_HOME", hostile.into()).unwrap_err();
            assert!(
                error.to_string().contains("control characters"),
                "{error:#}"
            );
        }
        #[cfg(unix)]
        assert!(validated_supervisor_fallback_home(PathBuf::from("/tmp/home\ninjected")).is_err());

        assert_eq!(validated_supervisor_pro_channel(None).unwrap(), None);
        for channel in ["stable", "staging"] {
            assert_eq!(
                validated_supervisor_pro_channel(Some(channel.into())).unwrap(),
                Some(channel.to_owned())
            );
        }
        for invalid in ["", "production", "STAGING", "staging "] {
            let error = validated_supervisor_pro_channel(Some(invalid.into())).unwrap_err();
            assert!(
                error.to_string().contains("must be stable or staging"),
                "{error:#}"
            );
        }
    }
}
