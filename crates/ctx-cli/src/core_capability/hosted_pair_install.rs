use std::{
    fs::{self, File, OpenOptions},
    io::{Read as _, Write as _},
    path::{Component, Path, PathBuf},
};

use anyhow::{anyhow, Context as _, Result};
use ctx_companion_bridge::{verify_signed_managed_pair_envelope, SignedManagedPairIdentity};
use ctx_upgrade_engine::{
    managed_install_marker_for_current_exe, ManagedInstallMarker, VerifiedManagedPairIdentity,
    MANAGED_PAIR_ENVELOPE_RELATIVE_PATH, MANAGED_PAIR_STATE_RELATIVE_PATH,
};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest as _, Sha256};

use super::{engine_identity, write_response_frame};

const MAX_HOSTED_MARKER_BYTES: u64 = 64 * 1024;

/// Completes hosted distribution from an already installed, signed Core.
/// Artifact acquisition remains in the thin installer; signature verification,
/// fixed-slot construction, rollback protection, and activation live here.
pub(super) fn run(arguments: &[std::ffi::OsString]) -> Result<()> {
    if arguments.len() != 6 {
        return Err(anyhow!("invalid hosted managed-pair install invocation"));
    }
    let envelope = hosted_source_path(&arguments[2], "signed envelope")?;
    let candidate_core = hosted_source_path(&arguments[3], "Core artifact")?;
    let candidate_companion = hosted_source_path(&arguments[4], "companion artifact")?;
    let candidate_marker = hosted_source_path(&arguments[5], "install marker")?;
    let core = std::env::current_exe().context("resolve installed Core executable")?;
    let expected_core_name = if cfg!(windows) { "ctx.exe" } else { "ctx" };
    let bin = core
        .parent()
        .filter(|path| path.file_name() == Some(std::ffi::OsStr::new("bin")))
        .filter(|_| core.file_name() == Some(std::ffi::OsStr::new(expected_core_name)))
        .ok_or_else(|| {
            anyhow!("hosted pair installation requires <root>/bin/{expected_core_name}")
        })?;
    let install_root = bin
        .parent()
        .ok_or_else(|| anyhow!("installed Core has no installation root"))?;
    let current_marker_path = hosted_marker_path(&core);
    let current_marker = read_hosted_marker(&current_marker_path, "installed Core marker")?;
    validate_hosted_marker_path(&current_marker, &core)?;
    let channel = current_marker.release_channel()?;
    let envelope_bytes = read_bounded_file(&envelope, 2 * 1024 * 1024, "signed envelope")?;
    let expectations = ctx_companion_bridge::ManagedPairExpectations::new(channel);
    let signed_identity = verify_signed_managed_pair_envelope(&expectations, &envelope_bytes)
        .map_err(|error| anyhow!(error.to_string()))?;
    verify_hosted_component(&candidate_core, signed_identity.core(), "Core artifact")?;
    verify_hosted_component(
        &candidate_companion,
        signed_identity.companion(),
        "companion artifact",
    )?;

    let strict_core_identity = matches!(
        managed_install_marker_for_current_exe()?,
        ManagedInstallMarker::Valid(_)
    );
    if !strict_core_identity {
        verify_hosted_component(&core, signed_identity.core(), "recovering installed Core")
            .context("hosted Core install identity is invalid")?;
    }

    let next_marker = read_hosted_marker(&candidate_marker, "candidate install marker")?;
    validate_hosted_marker_path(&next_marker, &core)?;
    if next_marker.release_channel()? != channel
        || next_marker.platform != current_marker.platform
        || next_marker.sha256 != signed_identity.core().sha256().to_hex()
    {
        return Err(anyhow!(
            "candidate install marker does not match the signed Core artifact"
        ));
    }
    let _lock = acquire_hosted_install_lock(install_root)?;
    reject_hosted_rollback(install_root, &expectations, &signed_identity)?;

    let installed_companion = install_root.join("libexec").join(if cfg!(windows) {
        "ctx-pro.exe"
    } else {
        "ctx-pro"
    });
    let installed_envelope = install_root.join(MANAGED_PAIR_ENVELOPE_RELATIVE_PATH);
    let installed_receipt = install_root.join(MANAGED_PAIR_STATE_RELATIVE_PATH);
    let identity = engine_identity(&signed_identity)?;
    let receipt = hosted_pair_receipt(&identity, &envelope_bytes)?;

    let staged_companion = stage_hosted_file(
        &candidate_companion,
        &installed_companion,
        0o755,
        "companion artifact",
    )?;
    let staged_core = stage_hosted_file_if_changed(
        &candidate_core,
        &core,
        signed_identity.core().size_bytes(),
        &signed_identity.core().sha256().to_hex(),
        0o755,
        "Core artifact",
    )?;
    let staged_envelope =
        stage_hosted_file(&envelope, &installed_envelope, 0o600, "signed envelope")?;
    let staged_marker = stage_hosted_file(
        &candidate_marker,
        &current_marker_path,
        0o600,
        "install marker",
    )?;
    let staged_receipt =
        stage_hosted_bytes(&receipt, &installed_receipt, 0o600, "install receipt")?;

    HostedPairPublication {
        staged_envelope: &staged_envelope,
        installed_envelope: &installed_envelope,
        staged_companion: &staged_companion,
        installed_companion: &installed_companion,
        staged_core: staged_core.as_deref(),
        installed_core: &core,
        staged_marker: &staged_marker,
        installed_marker: &current_marker_path,
        staged_receipt: &staged_receipt,
        installed_receipt: &installed_receipt,
    }
    .publish()?;

    let receipt = json!({
        "command": "hosted_managed_pair_install",
        "release_name": identity.release_name(),
        "rollback_generation": identity.rollback_generation(),
        "schema_version": 1,
        "status": "committed",
    });
    let receipt = serde_json::to_vec_pretty(&receipt)
        .context("serialize hosted managed-pair install receipt")?;
    write_response_frame(std::io::stdout().lock(), &receipt)
}

pub(super) fn hosted_source_path(value: &std::ffi::OsStr, label: &str) -> Result<PathBuf> {
    let path = PathBuf::from(value);
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return Err(anyhow!(
            "hosted {label} path must be normalized and absolute"
        ));
    }
    let metadata =
        std::fs::symlink_metadata(&path).with_context(|| format!("inspect hosted {label}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(anyhow!("hosted {label} must be a regular file"));
    }
    Ok(path)
}

#[derive(Debug, Deserialize)]
pub(super) struct HostedInstallMarker {
    pub(super) schema_version: u32,
    pub(super) manager: String,
    pub(super) install_path: String,
    pub(super) platform: String,
    pub(super) channel: String,
    pub(super) sha256: String,
    #[serde(default)]
    pub(super) staging_dogfood: bool,
}

impl HostedInstallMarker {
    pub(super) fn release_channel(&self) -> Result<ctx_companion_bridge::ReleaseChannel> {
        if self.staging_dogfood {
            Ok(ctx_companion_bridge::ReleaseChannel::Staging)
        } else if self.channel == "stable" {
            Ok(ctx_companion_bridge::ReleaseChannel::Stable)
        } else {
            Err(anyhow!("hosted install marker channel is unsupported"))
        }
    }
}

fn hosted_marker_path(core: &Path) -> PathBuf {
    let mut marker = core.as_os_str().to_owned();
    marker.push(".install.json");
    PathBuf::from(marker)
}

fn read_hosted_marker(path: &Path, label: &str) -> Result<HostedInstallMarker> {
    let bytes = read_bounded_file(path, MAX_HOSTED_MARKER_BYTES, label)?;
    let marker: HostedInstallMarker =
        serde_json::from_slice(&bytes).with_context(|| format!("parse {label}"))?;
    if marker.schema_version != 1
        || marker.manager != "ctx-hosted-installer"
        || marker.sha256.len() != 64
        || !marker.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(anyhow!("{label} is invalid"));
    }
    Ok(marker)
}

fn validate_hosted_marker_path(marker: &HostedInstallMarker, core: &Path) -> Result<()> {
    if Path::new(&marker.install_path) != core {
        return Err(anyhow!("hosted install marker does not own this Core path"));
    }
    Ok(())
}

fn read_bounded_file(path: &Path, maximum: u64, label: &str) -> Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path).with_context(|| format!("inspect {label}"))?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() == 0
        || metadata.len() > maximum
    {
        return Err(anyhow!("{label} is not a bounded regular file"));
    }
    fs::read(path).with_context(|| format!("read {label}"))
}

fn verify_hosted_component(
    path: &Path,
    expected: ctx_companion_bridge::SignedManagedPairComponentIdentity,
    label: &str,
) -> Result<()> {
    let metadata = fs::symlink_metadata(path).with_context(|| format!("inspect {label}"))?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() != expected.size_bytes()
        || sha256_file(path)? != expected.sha256().to_hex()
    {
        return Err(anyhow!("{label} does not match its signed identity"));
    }
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn reject_hosted_rollback(
    install_root: &Path,
    expectations: &ctx_companion_bridge::ManagedPairExpectations,
    candidate: &SignedManagedPairIdentity,
) -> Result<()> {
    let installed = install_root.join(MANAGED_PAIR_ENVELOPE_RELATIVE_PATH);
    let bytes = match fs::symlink_metadata(&installed) {
        Ok(_) => read_bounded_file(&installed, 2 * 1024 * 1024, "installed signed envelope")?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).context("inspect installed signed envelope"),
    };
    let current = verify_signed_managed_pair_envelope(expectations, &bytes)
        .map_err(|error| anyhow!(error.to_string()))?;
    if candidate.rollback_generation() < current.rollback_generation()
        || (candidate.rollback_generation() == current.rollback_generation()
            && candidate != &current)
    {
        return Err(anyhow!(
            "managed-pair rollback protection rejected the candidate"
        ));
    }
    Ok(())
}

fn hosted_pair_receipt(identity: &VerifiedManagedPairIdentity, envelope: &[u8]) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec_pretty(&json!({
        "contract": "ctx-managed-pair-state",
        "schema_version": 1,
        "identity": identity,
        "envelope_sha256": format!("{:x}", Sha256::digest(envelope)),
        "envelope_size_bytes": envelope.len(),
    }))?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn stage_hosted_file(source: &Path, target: &Path, mode: u32, label: &str) -> Result<PathBuf> {
    let bytes = fs::read(source).with_context(|| format!("read hosted {label}"))?;
    stage_hosted_bytes(&bytes, target, mode, label)
}

pub(super) fn stage_hosted_file_if_changed(
    source: &Path,
    target: &Path,
    expected_size: u64,
    expected_sha256: &str,
    mode: u32,
    label: &str,
) -> Result<Option<PathBuf>> {
    match fs::symlink_metadata(target) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Err(anyhow!("installed hosted {label} path is unsafe"));
        }
        Ok(metadata)
            if metadata.len() == expected_size && sha256_file(target)? == expected_sha256 =>
        {
            return Ok(None);
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).with_context(|| format!("inspect installed {label}")),
    }
    stage_hosted_file(source, target, mode, label).map(Some)
}

pub(super) struct HostedPairPublication<'a> {
    pub(super) staged_envelope: &'a Path,
    pub(super) installed_envelope: &'a Path,
    pub(super) staged_companion: &'a Path,
    pub(super) installed_companion: &'a Path,
    pub(super) staged_core: Option<&'a Path>,
    pub(super) installed_core: &'a Path,
    pub(super) staged_marker: &'a Path,
    pub(super) installed_marker: &'a Path,
    pub(super) staged_receipt: &'a Path,
    pub(super) installed_receipt: &'a Path,
}

impl HostedPairPublication<'_> {
    pub(super) fn publish(self) -> Result<()> {
        // The signed rollback watermark advances before binaries. Every
        // binary replacement remains usable because V3 makes old/new Core and
        // Pro interoperable; the receipt is evidence only, so it is last.
        replace_hosted_file(
            self.staged_envelope,
            self.installed_envelope,
            "signed envelope",
        )?;
        replace_hosted_file(
            self.staged_companion,
            self.installed_companion,
            "companion artifact",
        )?;
        if let Some(staged_core) = self.staged_core {
            replace_hosted_file(staged_core, self.installed_core, "Core artifact")?;
        }
        replace_hosted_file(self.staged_marker, self.installed_marker, "install marker")?;
        replace_hosted_file(
            self.staged_receipt,
            self.installed_receipt,
            "install receipt",
        )
    }
}

pub(super) fn stage_hosted_bytes(
    bytes: &[u8],
    target: &Path,
    mode: u32,
    label: &str,
) -> Result<PathBuf> {
    let parent = target
        .parent()
        .ok_or_else(|| anyhow!("hosted {label} target has no parent"))?;
    fs::create_dir_all(parent).with_context(|| format!("create hosted {label} directory"))?;
    if fs::symlink_metadata(parent)?.file_type().is_symlink() {
        return Err(anyhow!("hosted {label} directory must not be a symlink"));
    }
    let name = target
        .file_name()
        .ok_or_else(|| anyhow!("hosted {label} target has no file name"))?
        .to_string_lossy();
    let staged = parent.join(format!(".{name}.hosted-pair.new"));
    match fs::symlink_metadata(&staged) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            fs::remove_file(&staged).with_context(|| format!("remove stale staged {label}"))?;
        }
        Ok(_) => return Err(anyhow!("staged hosted {label} path is unsafe")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).with_context(|| format!("inspect staged {label}")),
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(mode);
    }
    let mut file = options
        .open(&staged)
        .with_context(|| format!("create staged hosted {label}"))?;
    file.write_all(bytes)
        .with_context(|| format!("write staged hosted {label}"))?;
    file.sync_all()
        .with_context(|| format!("sync staged hosted {label}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&staged, fs::Permissions::from_mode(mode))?;
    }
    Ok(staged)
}

pub(super) struct HostedInstallLock {
    _file: File,
}

pub(super) fn acquire_hosted_install_lock(install_root: &Path) -> Result<HostedInstallLock> {
    use fs2::FileExt as _;

    let control = install_root.join("share/ctx");
    fs::create_dir_all(&control).context("create hosted install control directory")?;
    if fs::symlink_metadata(&control)?.file_type().is_symlink() {
        return Err(anyhow!(
            "hosted install control directory must not be a symlink"
        ));
    }
    let path = control.join(".hosted-pair-install.lock");
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {}
        Ok(_) => return Err(anyhow!("hosted install lock path is unsafe")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("inspect hosted install lock"),
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let file = options.open(&path).context("open hosted install lock")?;
    file.try_lock_exclusive()
        .context("another hosted pair installation is active")?;
    Ok(HostedInstallLock { _file: file })
}

pub(super) fn replace_hosted_file(staged: &Path, target: &Path, label: &str) -> Result<()> {
    match fs::symlink_metadata(target) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {}
        Ok(_) => return Err(anyhow!("installed hosted {label} path is unsafe")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).with_context(|| format!("inspect installed {label}")),
    }
    fs::rename(staged, target).with_context(|| format!("atomically publish hosted {label}"))?;
    #[cfg(unix)]
    {
        let parent = target
            .parent()
            .ok_or_else(|| anyhow!("hosted {label} target has no parent"))?;
        File::open(parent)?
            .sync_all()
            .with_context(|| format!("sync hosted {label} directory"))?;
    }
    Ok(())
}
