use anyhow::{anyhow, Result};
use std::path::{Path, PathBuf};

use crate::NormalizedLaunch;

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct SupervisorIdentity {
    name: String,
    artifact_path: PathBuf,
}

impl SupervisorIdentity {
    pub fn new(name: impl Into<String>, artifact_path: PathBuf) -> Result<Self> {
        let name = name.into();
        let name = validated_supervisor_artifact_text("supervisor identity", &name)?;
        if name.is_empty() {
            return Err(anyhow!("supervisor identity may not be empty"));
        }
        Ok(Self {
            name: name.to_owned(),
            artifact_path,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn artifact_path(&self) -> &Path {
        &self.artifact_path
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct SupervisorSpec {
    identity: SupervisorIdentity,
    description: String,
    launch: NormalizedLaunch,
}

impl SupervisorSpec {
    pub fn new(
        identity: SupervisorIdentity,
        description: impl Into<String>,
        launch: NormalizedLaunch,
    ) -> Result<Self> {
        let description =
            validated_supervisor_artifact_text("service description", &description.into())?
                .to_owned();
        for (name, value) in launch.environment() {
            let name = name
                .to_str()
                .ok_or_else(|| anyhow!("supervisor environment name is not Unicode"))?;
            validated_supervisor_artifact_text("environment variable name", name)?;
            let value = value
                .to_str()
                .ok_or_else(|| anyhow!("supervisor environment value {name} is not Unicode"))?;
            validated_supervisor_artifact_text(&format!("environment variable {name}"), value)?;
        }
        Ok(Self {
            identity,
            description,
            launch,
        })
    }

    pub fn identity(&self) -> &SupervisorIdentity {
        &self.identity
    }

    pub fn description(&self) -> &str {
        &self.description
    }

    pub fn launch(&self) -> &NormalizedLaunch {
        &self.launch
    }
}

pub fn linux_systemd_unit(spec: &SupervisorSpec) -> Result<String> {
    let executable =
        validated_supervisor_artifact_path("daemon executable", spec.launch.program())?;
    let environment = spec
        .launch
        .environment()
        .map(|(name, value)| {
            let name = name.to_string_lossy();
            let value = value.to_string_lossy();
            systemd_quote_text(&format!("{name}={value}"))
        })
        .collect::<Vec<_>>()
        .join(" ");
    let args =
        spec.launch
            .args()
            .map(|arg| {
                let arg = arg
                    .to_str()
                    .ok_or_else(|| anyhow!("supervisor argument is not Unicode"))?;
                validated_supervisor_artifact_text("daemon argument", arg)?;
                Ok(
                    if arg.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'=')
                    }) {
                        arg.to_owned()
                    } else {
                        systemd_quote_text(arg)
                    },
                )
            })
            .collect::<Result<Vec<_>>>()?
            .join(" ");
    Ok(format!(
        "[Unit]\nDescription={}\n\n[Service]\nType=simple\nExecStart=/usr/bin/env -i {} {}{}\nRestart=on-failure\nRestartSec=2\nStandardOutput=null\nStandardError=journal\n\n[Install]\nWantedBy=default.target\n",
        spec.description(),
        environment,
        systemd_quote_text(executable),
        if args.is_empty() { String::new() } else { format!(" {args}") },
    ))
}

pub fn launch_agent_plist(spec: &SupervisorSpec) -> Result<String> {
    let executable =
        validated_supervisor_artifact_path("daemon executable", spec.launch.program())?;
    let environment = spec
        .launch
        .environment()
        .map(|(name, value)| {
            format!(
                "<string>{}</string>",
                xml_escape(&format!(
                    "{}={}",
                    name.to_string_lossy(),
                    value.to_string_lossy()
                ))
            )
        })
        .collect::<Vec<_>>()
        .join("");
    let args = spec
        .launch
        .args()
        .map(|arg| {
            let arg = arg
                .to_str()
                .ok_or_else(|| anyhow!("supervisor argument is not Unicode"))?;
            validated_supervisor_artifact_text("daemon argument", arg)?;
            Ok(format!("<string>{}</string>", xml_escape(arg)))
        })
        .collect::<Result<Vec<_>>>()?
        .join("");
    Ok(format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict>\n<key>Label</key><string>{}</string>\n<key>ProgramArguments</key><array><string>/usr/bin/env</string><string>-i</string>{}<string>{}</string>{}</array>\n<key>RunAtLoad</key><true/>\n<key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>\n<key>ProcessType</key><string>Background</string>\n<key>StandardOutPath</key><string>/dev/null</string>\n<key>StandardErrorPath</key><string>/dev/null</string>\n</dict></plist>\n",
        xml_escape(spec.identity().name()),
        environment,
        xml_escape(executable),
        args,
    ))
}

fn systemd_quote_text(value: &str) -> String {
    let value = value.replace('%', "%%");
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

pub fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

pub fn validated_supervisor_artifact_path<'a>(
    label: &str,
    path: &'a std::path::Path,
) -> Result<&'a str> {
    let value = path.to_str().ok_or_else(|| {
        anyhow!("supervisor {label} is not Unicode and cannot be persisted safely")
    })?;
    validated_supervisor_artifact_text(label, value)
}

pub fn validated_supervisor_artifact_text<'a>(label: &str, value: &'a str) -> Result<&'a str> {
    if value.chars().any(char::is_control) {
        return Err(anyhow!(
            "supervisor {label} contains control characters and cannot be persisted safely"
        ));
    }
    Ok(value)
}
