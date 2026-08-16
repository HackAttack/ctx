use crate::BridgeError;

#[derive(Clone, Copy)]
pub(super) struct TargetSpec {
    pub(super) id: &'static str,
    pub(super) os: &'static str,
    pub(super) arch: &'static str,
    pub(super) core_rust_target: &'static str,
    pub(super) companion_rust_target: &'static str,
    pub(super) core_slot: &'static str,
    pub(super) companion_slot: &'static str,
    pub(super) core_artifact: &'static str,
    pub(super) companion_artifact: &'static str,
}

impl TargetSpec {
    pub(super) fn current() -> Result<Self, BridgeError> {
        current_target()
    }
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn current_target() -> Result<TargetSpec, BridgeError> {
    Ok(TargetSpec {
        id: "linux-x64",
        os: "linux",
        arch: "x86_64",
        core_rust_target: "x86_64-unknown-linux-gnu",
        companion_rust_target: "x86_64-unknown-linux-gnu",
        core_slot: "<install-root>/bin/ctx",
        companion_slot: "<install-root>/libexec/ctx-pro",
        core_artifact: "ctx-linux-x64",
        companion_artifact: "ctx-pro-linux-x64",
    })
}

#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
fn current_target() -> Result<TargetSpec, BridgeError> {
    Ok(TargetSpec {
        id: "linux-arm64",
        os: "linux",
        arch: "aarch64",
        core_rust_target: "aarch64-unknown-linux-gnu",
        companion_rust_target: "aarch64-unknown-linux-gnu",
        core_slot: "<install-root>/bin/ctx",
        companion_slot: "<install-root>/libexec/ctx-pro",
        core_artifact: "ctx-linux-aarch64",
        companion_artifact: "ctx-pro-linux-arm64",
    })
}

#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
fn current_target() -> Result<TargetSpec, BridgeError> {
    Ok(TargetSpec {
        id: "macos-x64",
        os: "macos",
        arch: "x86_64",
        core_rust_target: "x86_64-apple-darwin",
        companion_rust_target: "x86_64-apple-darwin",
        core_slot: "<install-root>/bin/ctx",
        companion_slot: "<install-root>/libexec/ctx-pro",
        core_artifact: "ctx-macos-x64",
        companion_artifact: "ctx-pro-macos-x64",
    })
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn current_target() -> Result<TargetSpec, BridgeError> {
    Ok(TargetSpec {
        id: "macos-arm64",
        os: "macos",
        arch: "aarch64",
        core_rust_target: "aarch64-apple-darwin",
        companion_rust_target: "aarch64-apple-darwin",
        core_slot: "<install-root>/bin/ctx",
        companion_slot: "<install-root>/libexec/ctx-pro",
        core_artifact: "ctx-macos-arm64",
        companion_artifact: "ctx-pro-macos-arm64",
    })
}

#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
fn current_target() -> Result<TargetSpec, BridgeError> {
    Ok(TargetSpec {
        id: "windows-x64",
        os: "windows",
        arch: "x86_64",
        core_rust_target: "x86_64-pc-windows-gnu",
        companion_rust_target: "x86_64-pc-windows-msvc",
        core_slot: "<install-root>/bin/ctx.exe",
        companion_slot: "<install-root>/libexec/ctx-pro.exe",
        core_artifact: "ctx-windows-x64.exe",
        companion_artifact: "ctx-pro-windows-x64.exe",
    })
}

#[cfg(not(any(
    all(target_os = "linux", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "aarch64"),
    all(target_os = "macos", target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "aarch64"),
    all(target_os = "windows", target_arch = "x86_64")
)))]
fn current_target() -> Result<TargetSpec, BridgeError> {
    Err(BridgeError::UnsupportedPlatform)
}
