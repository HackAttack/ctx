use std::{
    collections::HashSet,
    fmt,
    fs::{self, File, Metadata, OpenOptions},
    io::Read,
    path::{Path, PathBuf},
};

use serde::{
    de::{Error as _, SeqAccess, Visitor},
    Deserialize, Deserializer, Serialize,
};
use tantivy::directory::Directory as _;

use crate::retention::{
    ensure_generation_read_lease_coordinator, try_generation_directory_reclaim_authority,
};
use crate::{
    active_index_files, load_active_generation_pointer, manifest_path, physical_integrity_audit,
    slot_path, sync_directory, ActiveGenerationPointer, DurableMmapDirectory,
    GenerationError as IndexError, GenerationRetentionLease, GenerationSlot,
    PhysicalIntegrityAudit, Result, INDEX_GENERATIONS_DIRECTORY, MANIFEST_DIRECTORY,
};

#[cfg(not(windows))]
const CERTIFICATION_VERSION: u32 = 3;
#[cfg(windows)]
const CERTIFICATION_VERSION: u32 = 4;
const CERTIFICATION_SUFFIX: &str = ".physical-certification.json";
const CERTIFICATION_DIRECTORY: &str = "integrity-certifications";
const TANTIVY_META_FILE: &str = "meta.json";
#[cfg(windows)]
const MANAGED_FILE: &str = ".managed.json";
const ARTIFACT_STABLE_SNAPSHOT_ATTEMPTS: usize = 4;
pub const MAX_CERTIFICATION_BYTES: usize = 1024 * 1024;
pub const MAX_CERTIFIED_ARTIFACTS: usize = 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GenerationIntegrityCertification {
    version: u32,
    pointer: ActiveGenerationPointer,
    pointer_identity: FileIdentity,
    manifest_identity: FileIdentity,
    slot: GenerationSlot,
    #[serde(deserialize_with = "deserialize_artifacts")]
    artifacts: Vec<CertifiedArtifact>,
}

/// In-memory proof that every file in one pointer-bound generation matched
/// the slot's expected physical SHA. The proof retains per-file digests so an
/// already-required candidate audit can authenticate managed hard-link
/// transitions without another full read of the active generation.
pub struct CertifiedPhysicalIntegrity {
    certification: GenerationIntegrityCertification,
}

impl CertifiedPhysicalIntegrity {
    pub(crate) fn certified_artifact(
        &self,
        path: &Path,
    ) -> Option<(ArtifactIdentity, [u8; 32], bool)> {
        let path = path.to_str()?;
        self.certification
            .artifacts
            .iter()
            .find(|artifact| artifact.artifact.path == path)
            .map(|artifact| (artifact.artifact.clone(), artifact.sha256, artifact.sealed))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CertifiedArtifact {
    #[serde(flatten)]
    artifact: ArtifactIdentity,
    sha256: [u8; 32],
    #[serde(default)]
    sealed: bool,
}

fn deserialize_artifacts<'de, D>(
    deserializer: D,
) -> std::result::Result<Vec<CertifiedArtifact>, D::Error>
where
    D: Deserializer<'de>,
{
    struct BoundedArtifacts;

    impl<'de> Visitor<'de> for BoundedArtifacts {
        type Value = Vec<CertifiedArtifact>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(
                formatter,
                "at most {MAX_CERTIFIED_ARTIFACTS} certified artifacts"
            )
        }

        fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
        where
            A: SeqAccess<'de>,
        {
            let hinted = sequence.size_hint().unwrap_or(0);
            if hinted > MAX_CERTIFIED_ARTIFACTS {
                return Err(A::Error::custom(
                    "certification artifact count exceeds bound",
                ));
            }
            let mut artifacts = Vec::with_capacity(hinted.min(MAX_CERTIFIED_ARTIFACTS));
            while let Some(artifact) = sequence.next_element()? {
                if artifacts.len() == MAX_CERTIFIED_ARTIFACTS {
                    return Err(A::Error::custom(
                        "certification artifact count exceeds bound",
                    ));
                }
                artifacts.push(artifact);
            }
            Ok(artifacts)
        }
    }

    deserializer.deserialize_seq(BoundedArtifacts)
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ArtifactIdentity {
    pub(super) path: String,
    pub(super) identity: FileIdentity,
}

impl ArtifactIdentity {
    /// Returns whether both observations still bind the same native file and
    /// immutable-content metadata, but some stronger identity metadata changed.
    ///
    /// This is a fail-closed concurrency classification, not proof that a hard
    /// link operation was the cause: ctime is excluded because managed
    /// link/unlink operations change it, and a same-size mutation with restored
    /// mtime must likewise force the caller to retry rather than accept bytes.
    pub(super) fn same_payload_identity_changed(&self, other: &Self) -> bool {
        self.path == other.path
            && self.identity != other.identity
            && self.identity.same_payload_identity(&other.identity)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct FileIdentity {
    length: u64,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(unix)]
    mode: u32,
    #[cfg(unix)]
    modified_seconds: i64,
    #[cfg(unix)]
    modified_nanoseconds: i64,
    #[cfg(unix)]
    changed_seconds: i64,
    #[cfg(unix)]
    changed_nanoseconds: i64,
    #[cfg(unix)]
    links: u64,
    #[cfg(windows)]
    volume_serial_number: u64,
    #[cfg(windows)]
    file_id: [u8; 16],
    #[cfg(windows)]
    creation_time: i64,
    #[cfg(windows)]
    last_write_time: i64,
    #[cfg(windows)]
    change_time: i64,
    #[cfg(windows)]
    attributes: u32,
    #[cfg(windows)]
    links: u32,
}

impl FileIdentity {
    pub(super) fn length(&self) -> u64 {
        self.length
    }

    fn link_count(&self) -> u64 {
        #[cfg(unix)]
        {
            self.links
        }
        #[cfg(windows)]
        {
            u64::from(self.links)
        }
        #[cfg(not(any(unix, windows)))]
        {
            0
        }
    }

    fn is_readonly(&self) -> bool {
        #[cfg(unix)]
        {
            self.mode & 0o222 == 0
        }
        #[cfg(windows)]
        {
            self.attributes & 0x1 != 0
        }
        #[cfg(not(any(unix, windows)))]
        {
            false
        }
    }

    fn same_native_file(&self, other: &Self) -> bool {
        #[cfg(unix)]
        {
            self.device == other.device && self.inode == other.inode
        }
        #[cfg(windows)]
        {
            self.volume_serial_number == other.volume_serial_number && self.file_id == other.file_id
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = other;
            false
        }
    }

    /// Identity fields that cannot be changed by creating or removing a hard
    /// link to the same immutable payload. Link operations may change ctime
    /// and link count, so those fields are deliberately excluded here.
    fn same_payload_identity(&self, other: &Self) -> bool {
        if !self.same_native_file(other) {
            return false;
        }
        #[cfg(unix)]
        {
            self.length == other.length
                && self.mode == other.mode
                && self.modified_seconds == other.modified_seconds
                && self.modified_nanoseconds == other.modified_nanoseconds
        }
        #[cfg(windows)]
        {
            self.length == other.length
                && self.creation_time == other.creation_time
                && self.last_write_time == other.last_write_time
                && self.attributes == other.attributes
        }
        #[cfg(not(any(unix, windows)))]
        {
            false
        }
    }

    #[cfg(windows)]
    fn follows_readonly_seal(&self, prior: &Self) -> bool {
        self.same_native_file(prior)
            && !prior.is_readonly()
            && self.is_readonly()
            && self.length == prior.length
            && self.creation_time == prior.creation_time
            && self.last_write_time == prior.last_write_time
            && self.links == prior.links
            && (self.attributes & !0x1) == (prior.attributes & !0x1)
    }
}

pub fn verify_or_certify_physical_integrity(
    root: &Path,
    pointer: &ActiveGenerationPointer,
    slot: &GenerationSlot,
    index: &tantivy::Index,
) -> Result<CertifiedPhysicalIntegrity> {
    if let Some(certification) = matching_certification(root, pointer, slot, index)? {
        return Ok(CertifiedPhysicalIntegrity { certification });
    }

    let generation_path = slot_path(root, slot);
    let audit = physical_integrity_audit(index, &generation_path, Some(pointer))?;
    if audit.digest() != slot.physical_integrity_digest() {
        return Err(IndexError::ChecksumMismatch);
    }
    install_certification(root, pointer, slot, index, &audit, false)
}

/// Verifies one immutable generation from its existing publication-time
/// certification without hashing artifact bodies or changing durable state.
///
/// The certification remains bound to the exact slot, manifest file, artifact
/// path set, and exact native files after the active pointer moves on. Any
/// metadata transition invalidates the inherited SHA authority because a
/// later link/unlink can mask an intervening same-size, restored-mtime write.
/// Missing, malformed, stale, or otherwise unsupported certification fails
/// closed without hashing artifact bodies.
pub fn verify_physical_integrity_read_only(
    root: &Path,
    slot: &GenerationSlot,
    index: &tantivy::Index,
) -> Result<()> {
    ensure_real_directory(root)?;
    ensure_real_directory(&root.join(MANIFEST_DIRECTORY))?;
    ensure_real_directory(&root.join(INDEX_GENERATIONS_DIRECTORY))?;
    let generation_path = slot_path(root, slot);
    ensure_real_directory(&generation_path)?;
    if crate::read_root::has_retained_read_authority(root, slot.generation_id()) {
        return crate::verify_physical_integrity(
            index,
            &generation_path,
            None,
            slot.physical_integrity_digest(),
        );
    }
    ensure_real_directory(&root.join(CERTIFICATION_DIRECTORY))?;

    let bytes =
        read_certification(&certification_path(root, slot)).ok_or(IndexError::ChecksumMismatch)?;
    let certification = serde_json::from_slice::<GenerationIntegrityCertification>(&bytes)
        .map_err(|_| IndexError::ChecksumMismatch)?;
    if serde_json::to_vec(&certification)? != bytes
        || certification.version != CERTIFICATION_VERSION
        || certification.slot != *slot
        || capture_single_link_control(&manifest_path(root, slot.generation_id()))?
            != certification.manifest_identity
    {
        return Err(IndexError::ChecksumMismatch);
    }

    let expected_paths = expected_artifact_paths(index)?;
    if certification
        .artifacts
        .iter()
        .map(|artifact| artifact.artifact.path.clone())
        .collect::<Vec<_>>()
        != expected_paths
    {
        return Err(IndexError::ChecksumMismatch);
    }
    let current_pointer = load_current_pointer(root)?;
    let retained_alias_directories = std::iter::once(current_pointer.active().directory())
        .chain(current_pointer.previous().map(GenerationSlot::directory))
        .chain(std::iter::once(slot.directory()))
        .map(str::to_owned)
        .collect::<HashSet<_>>();
    for expected in &certification.artifacts {
        let current = capture_artifact_with_retained_aliases(
            root,
            &generation_path,
            Path::new(&expected.artifact.path),
            &retained_alias_directories,
        )?;
        if current != expected.artifact {
            return Err(IndexError::ChecksumMismatch);
        }
    }
    if load_current_pointer(root)? != current_pointer {
        return Err(IndexError::ConcurrentGenerationChange);
    }
    Ok(())
}

pub fn scrub_and_certify_physical_integrity(
    root: &Path,
    pointer: &ActiveGenerationPointer,
    slot: &GenerationSlot,
    index: &tantivy::Index,
) -> Result<CertifiedPhysicalIntegrity> {
    let generation_path = slot_path(root, slot);
    let audit = physical_integrity_audit(index, &generation_path, Some(pointer))?;
    if audit.digest() != slot.physical_integrity_digest() {
        return Err(IndexError::ChecksumMismatch);
    }
    install_certification(root, pointer, slot, index, &audit, false)
}

/// Installs the certification for a candidate that was fully hashed before
/// pointer publication. Every artifact identity must still match the audit.
/// A hard-link transition invalidates the fast-path certification because it
/// can mask a same-size, restored-mtime write.
pub fn certify_activated_generation(
    root: &Path,
    pointer: &ActiveGenerationPointer,
    slot: &GenerationSlot,
    index: &tantivy::Index,
    audit: &PhysicalIntegrityAudit,
) -> Result<()> {
    install_certification(root, pointer, slot, index, audit, true).map(|_| ())
}

fn install_certification(
    root: &Path,
    pointer: &ActiveGenerationPointer,
    slot: &GenerationSlot,
    index: &tantivy::Index,
    audit: &PhysicalIntegrityAudit,
    allow_readonly_seal: bool,
) -> Result<CertifiedPhysicalIntegrity> {
    if load_current_pointer(root)? != *pointer {
        return Err(IndexError::ConcurrentGenerationChange);
    }
    let expected_paths = expected_artifact_paths(index)?;
    if audit.artifact_paths() != expected_paths {
        return Err(IndexError::ChecksumMismatch);
    }

    let generation_path = slot_path(root, slot);
    ensure_real_directory(root)?;
    ensure_real_directory(&root.join(MANIFEST_DIRECTORY))?;
    ensure_real_directory(&root.join(INDEX_GENERATIONS_DIRECTORY))?;
    ensure_real_directory(&generation_path)?;

    let pointer_identity = capture_pointer_bound_single_link_control(
        root,
        pointer,
        &root.join("active-generation.json"),
    )?;
    let manifest_identity = capture_pointer_bound_single_link_control(
        root,
        pointer,
        &manifest_path(root, slot.generation_id()),
    )?;
    let mut artifacts = Vec::with_capacity(audit.files().len());
    for prior in audit.files() {
        let mut current = capture_artifact(
            root,
            &generation_path,
            Path::new(&prior.artifact.path),
            Some(pointer),
        )?;
        let follows_allowed_seal = allow_readonly_seal && {
            #[cfg(windows)]
            {
                current
                    .identity
                    .follows_readonly_seal(&prior.artifact.identity)
            }
            #[cfg(not(windows))]
            {
                false
            }
        };
        if current.identity != prior.artifact.identity && !follows_allowed_seal {
            return if prior.artifact.same_payload_identity_changed(&current) {
                Err(IndexError::ConcurrentGenerationChange)
            } else {
                Err(IndexError::ChecksumMismatch)
            };
        }
        let sealed = artifact_should_be_sealed(&prior.artifact.path);
        if sealed {
            current = seal_artifact(
                root,
                &generation_path,
                Path::new(&prior.artifact.path),
                Some(pointer),
                &current,
            )?;
        }
        artifacts.push(CertifiedArtifact {
            artifact: current,
            sha256: prior.sha256,
            sealed,
        });
    }
    let certification = GenerationIntegrityCertification {
        version: CERTIFICATION_VERSION,
        pointer: pointer.clone(),
        pointer_identity,
        manifest_identity,
        slot: slot.clone(),
        artifacts,
    };
    if certification.artifacts.len() <= MAX_CERTIFIED_ARTIFACTS {
        let bytes = serde_json::to_vec(&certification)?;
        if bytes.len() <= MAX_CERTIFICATION_BYTES {
            let certification_directory = root.join(CERTIFICATION_DIRECTORY);
            if fs::create_dir_all(&certification_directory).is_ok()
                && ensure_real_directory(&certification_directory).is_ok()
            {
                if let Ok(directory) = DurableMmapDirectory::open(root) {
                    let relative_path =
                        Path::new(CERTIFICATION_DIRECTORY).join(certification_file_name(slot));
                    if directory.atomic_write(&relative_path, &bytes).is_ok()
                        && matching_certification(root, pointer, slot, index)?.is_none()
                    {
                        return Err(IndexError::ConcurrentGenerationChange);
                    }
                }
            }
        }
    }
    Ok(CertifiedPhysicalIntegrity { certification })
}

fn seal_artifact(
    root: &Path,
    generation_path: &Path,
    relative_path: &Path,
    pointer: Option<&ActiveGenerationPointer>,
    expected: &ArtifactIdentity,
) -> Result<ArtifactIdentity> {
    let (file, observed) = open_artifact(root, generation_path, relative_path, pointer)?;
    #[cfg(windows)]
    let _ = &file;
    if observed != *expected {
        return Err(IndexError::ConcurrentGenerationChange);
    }
    #[cfg(windows)]
    let expected_sealed_identity = if observed.identity.is_readonly() {
        observed.identity.clone()
    } else {
        seal_unsealed_artifact(&generation_path.join(relative_path), &observed)?
    };
    if !observed.identity.is_readonly() {
        #[cfg(not(windows))]
        let mut permissions = file.metadata()?.permissions();
        #[cfg(not(windows))]
        permissions.set_readonly(true);
        #[cfg(not(windows))]
        file.set_permissions(permissions)?;
        #[cfg(not(windows))]
        file.sync_all()?;
    }
    let sealed = recapture_artifact(root, generation_path, relative_path, pointer)?;
    #[cfg(windows)]
    if sealed.identity != expected_sealed_identity {
        return Err(IndexError::ConcurrentGenerationChange);
    }
    if !sealed.identity.is_readonly()
        || !sealed.identity.same_native_file(&observed.identity)
        || sealed.identity.length() != observed.identity.length()
    {
        return Err(IndexError::ConcurrentGenerationChange);
    }
    Ok(sealed)
}

#[cfg(windows)]
fn seal_unsealed_artifact(path: &Path, expected: &ArtifactIdentity) -> Result<FileIdentity> {
    use std::os::windows::fs::OpenOptionsExt as _;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_SHARE_READ,
    };

    validate_named_regular_file(path)?;
    let file = OpenOptions::new()
        .access_mode(FILE_GENERIC_READ | FILE_GENERIC_WRITE)
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    if file_identity(&file)? != expected.identity {
        return Err(IndexError::ConcurrentGenerationChange);
    }
    validate_named_regular_file(path)?;
    let named = open_nofollow(path).map_err(|_| IndexError::ConcurrentGenerationChange)?;
    if file_identity(&named)? != expected.identity {
        return Err(IndexError::ConcurrentGenerationChange);
    }
    let mut permissions = file.metadata()?.permissions();
    permissions.set_readonly(true);
    file.set_permissions(permissions)?;
    file.sync_all()?;
    let sealed = file_identity(&file)?;
    if !sealed.follows_readonly_seal(&expected.identity) {
        return Err(IndexError::ConcurrentGenerationChange);
    }
    Ok(sealed)
}

#[cfg(windows)]
fn artifact_should_be_sealed(_path: &str) -> bool {
    true
}

#[cfg(not(windows))]
fn artifact_should_be_sealed(path: &str) -> bool {
    path != TANTIVY_META_FILE
}

#[cfg(windows)]
struct TerminalSealEntry {
    relative_path: PathBuf,
    file: File,
    before: ArtifactIdentity,
    sealed: ArtifactIdentity,
}

/// Keeps every Windows candidate artifact open from its terminal read-only
/// seal through the active-pointer replacement.
#[cfg(windows)]
pub struct TerminalPublicationGuard {
    root: PathBuf,
    generation_path: PathBuf,
    topology_authority: Option<ActiveGenerationPointer>,
    entries: Vec<TerminalSealEntry>,
}

#[cfg(windows)]
pub fn acquire_terminal_publication_guard(
    root: &Path,
    generation_path: &Path,
    index: &tantivy::Index,
    topology_authority: Option<&ActiveGenerationPointer>,
) -> Result<TerminalPublicationGuard> {
    ensure_real_directory(root)?;
    ensure_real_directory(&root.join(INDEX_GENERATIONS_DIRECTORY))?;
    ensure_real_directory(generation_path)?;

    let mut allowlist = active_index_files(index)?;
    allowlist.insert(PathBuf::from(TANTIVY_META_FILE));
    allowlist.insert(PathBuf::from(MANAGED_FILE));
    if allowlist.len() > MAX_CERTIFIED_ARTIFACTS.saturating_add(1) {
        return Err(IndexError::ChecksumMismatch);
    }

    let mut entries = allowlist
        .into_iter()
        .map(|relative_path| {
            open_terminal_seal_entry(root, generation_path, relative_path, topology_authority)
        })
        .collect::<Result<Vec<_>>>()?;

    for entry in &mut entries {
        let mut permissions = entry.file.metadata()?.permissions();
        permissions.set_readonly(true);
        entry.file.set_permissions(permissions)?;
        entry.file.sync_all()?;
        let sealed_identity = file_identity(&entry.file)?;
        if !sealed_identity.follows_readonly_seal(&entry.before.identity) {
            return Err(IndexError::ConcurrentGenerationChange);
        }
        let sealed = recapture_authenticated_artifact(
            root,
            generation_path,
            &entry.relative_path,
            &entry.file,
            topology_authority,
        )?;
        if sealed.identity != sealed_identity {
            return Err(IndexError::ConcurrentGenerationChange);
        }
        entry.sealed = sealed;
    }

    Ok(TerminalPublicationGuard {
        root: root.to_owned(),
        generation_path: generation_path.to_owned(),
        topology_authority: topology_authority.cloned(),
        entries,
    })
}

#[cfg(windows)]
fn open_terminal_seal_entry(
    root: &Path,
    generation_path: &Path,
    relative_path: PathBuf,
    topology_authority: Option<&ActiveGenerationPointer>,
) -> Result<TerminalSealEntry> {
    use std::os::windows::fs::OpenOptionsExt as _;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_SHARE_READ,
    };

    if relative_path.components().count() != 1 {
        return Err(IndexError::ChecksumMismatch);
    }
    let path = generation_path.join(&relative_path);
    validate_named_regular_file(&path)?;
    let file = OpenOptions::new()
        .access_mode(FILE_GENERIC_READ | FILE_GENERIC_WRITE)
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(&path)?;
    let opened = file_identity(&file)?;
    if opened.is_readonly() {
        return Err(IndexError::ConcurrentGenerationChange);
    }
    validate_named_regular_file(&path)?;
    let named = open_nofollow(&path).map_err(|_| IndexError::ConcurrentGenerationChange)?;
    if file_identity(&named)? != opened || file_identity(&file)? != opened {
        return Err(IndexError::ConcurrentGenerationChange);
    }
    let before = recapture_authenticated_artifact(
        root,
        generation_path,
        &relative_path,
        &file,
        topology_authority,
    )?;
    if before.identity != opened {
        return Err(IndexError::ConcurrentGenerationChange);
    }
    Ok(TerminalSealEntry {
        relative_path,
        file,
        sealed: before.clone(),
        before,
    })
}

#[cfg(windows)]
impl TerminalPublicationGuard {
    pub fn verify_physical_fence(&self, expected: &PhysicalIntegrityAudit) -> Result<()> {
        if self.entries.len() != expected.files().len().saturating_add(1) {
            return Err(IndexError::ChecksumMismatch);
        }
        for expected_file in expected.files() {
            let Some(entry) = self
                .entries
                .iter()
                .find(|entry| entry.before.path == expected_file.artifact.path)
            else {
                return Err(IndexError::ChecksumMismatch);
            };
            if entry.before != expected_file.artifact
                || !entry
                    .sealed
                    .identity
                    .follows_readonly_seal(&entry.before.identity)
            {
                return Err(IndexError::ConcurrentGenerationChange);
            }
        }
        Ok(())
    }

    pub fn verify_identities(&self) -> Result<()> {
        for entry in &self.entries {
            let current = recapture_authenticated_artifact(
                &self.root,
                &self.generation_path,
                &entry.relative_path,
                &entry.file,
                self.topology_authority.as_ref(),
            )?;
            if current != entry.sealed {
                return Err(IndexError::ConcurrentGenerationChange);
            }
        }
        Ok(())
    }
}

fn matching_certification(
    root: &Path,
    pointer: &ActiveGenerationPointer,
    slot: &GenerationSlot,
    index: &tantivy::Index,
) -> Result<Option<GenerationIntegrityCertification>> {
    if ensure_real_directory(&root.join(CERTIFICATION_DIRECTORY)).is_err() {
        return Ok(None);
    }
    let Some(bytes) = read_certification(&certification_path(root, slot)) else {
        return Ok(None);
    };
    let Ok(certification) = serde_json::from_slice::<GenerationIntegrityCertification>(&bytes)
    else {
        return Ok(None);
    };
    if serde_json::to_vec(&certification)? != bytes
        || certification.version != CERTIFICATION_VERSION
        || certification.pointer != *pointer
        || certification.slot != *slot
    {
        return Ok(None);
    }
    if load_current_pointer(root)? != *pointer {
        return Err(IndexError::ConcurrentGenerationChange);
    }

    ensure_real_directory(root)?;
    ensure_real_directory(&root.join(MANIFEST_DIRECTORY))?;
    ensure_real_directory(&root.join(INDEX_GENERATIONS_DIRECTORY))?;
    let generation_path = slot_path(root, slot);
    ensure_real_directory(&generation_path)?;
    if capture_pointer_bound_single_link_control(
        root,
        pointer,
        &root.join("active-generation.json"),
    )? != certification.pointer_identity
        || capture_pointer_bound_single_link_control(
            root,
            pointer,
            &manifest_path(root, slot.generation_id()),
        )? != certification.manifest_identity
    {
        return Ok(None);
    }

    let expected_paths = expected_artifact_paths(index)?;
    if certification
        .artifacts
        .iter()
        .map(|artifact| artifact.artifact.path.clone())
        .collect::<Vec<_>>()
        != expected_paths
    {
        return Ok(None);
    }
    for expected in &certification.artifacts {
        let current = capture_artifact(
            root,
            &generation_path,
            Path::new(&expected.artifact.path),
            Some(pointer),
        )?;
        if current != expected.artifact {
            return Ok(None);
        }
    }
    if load_current_pointer(root)? != *pointer {
        return Err(IndexError::ConcurrentGenerationChange);
    }
    Ok(Some(certification))
}

/// Revalidates one in-memory expected-SHA proof immediately before a writer
/// relies on retained base artifacts. Exact identities take the metadata-only
/// fast path. A managed hard-link transition is accepted only when the
/// candidate's already-required physical audit observed the same native file
/// and the same per-file SHA that was authenticated for the active base.
pub fn verify_certified_physical_integrity(
    root: &Path,
    pointer: &ActiveGenerationPointer,
    slot: &GenerationSlot,
    certified: &CertifiedPhysicalIntegrity,
    candidate_audit: Option<&PhysicalIntegrityAudit>,
) -> Result<()> {
    let certification = &certified.certification;
    if certification.pointer != *pointer || certification.slot != *slot {
        return Err(IndexError::ConcurrentGenerationChange);
    }
    if load_current_pointer(root)? != *pointer {
        return Err(IndexError::ConcurrentGenerationChange);
    }
    ensure_real_directory(root)?;
    ensure_real_directory(&root.join(MANIFEST_DIRECTORY))?;
    ensure_real_directory(&root.join(INDEX_GENERATIONS_DIRECTORY))?;
    let generation_path = slot_path(root, slot);
    ensure_real_directory(&generation_path)?;
    if capture_pointer_bound_single_link_control(
        root,
        pointer,
        &root.join("active-generation.json"),
    )? != certification.pointer_identity
        || capture_pointer_bound_single_link_control(
            root,
            pointer,
            &manifest_path(root, slot.generation_id()),
        )? != certification.manifest_identity
    {
        return Err(IndexError::ConcurrentGenerationChange);
    }

    for expected in &certification.artifacts {
        let current = capture_artifact(
            root,
            &generation_path,
            Path::new(&expected.artifact.path),
            Some(pointer),
        )?;
        let candidate_file = candidate_audit.and_then(|audit| {
            audit
                .files()
                .iter()
                .find(|file| file.artifact.path == expected.artifact.path)
        });
        if candidate_audit.is_none() || expected.artifact.path == TANTIVY_META_FILE {
            if current != expected.artifact {
                return Err(IndexError::ChecksumMismatch);
            }
            continue;
        }
        let Some(candidate_file) = candidate_file else {
            // This segment is absent from the candidate and therefore cannot
            // be used as an exhaustive-verification exclusion.
            continue;
        };
        if candidate_file.sha256 != expected.sha256 {
            return Err(IndexError::ChecksumMismatch);
        }
        if current != expected.artifact
            && (!expected.artifact.same_payload_identity_changed(&current)
                || candidate_file.artifact != current)
        {
            return Err(IndexError::ChecksumMismatch);
        }
    }
    if load_current_pointer(root)? != *pointer {
        return Err(IndexError::ConcurrentGenerationChange);
    }
    Ok(())
}

fn load_current_pointer(root: &Path) -> Result<ActiveGenerationPointer> {
    load_active_generation_pointer(root)?.ok_or(IndexError::MissingActiveGenerationPointer)
}

fn expected_artifact_paths(index: &tantivy::Index) -> Result<Vec<String>> {
    let mut paths = active_index_files(index)?;
    paths.insert(PathBuf::from(TANTIVY_META_FILE));
    paths
        .into_iter()
        .map(|path| {
            path.to_str()
                .map(str::to_owned)
                .ok_or(IndexError::ChecksumMismatch)
        })
        .collect()
}

fn read_certification(path: &Path) -> Option<Vec<u8>> {
    let (file, identity) = open_regular_file(path).ok()?;
    let length = usize::try_from(identity.length()).ok()?;
    if length > MAX_CERTIFICATION_BYTES {
        return None;
    }
    let mut bytes = Vec::with_capacity(length);
    file.take(
        u64::try_from(MAX_CERTIFICATION_BYTES)
            .ok()?
            .saturating_add(1),
    )
    .read_to_end(&mut bytes)
    .ok()?;
    if bytes.len() > MAX_CERTIFICATION_BYTES || bytes.len() != length {
        return None;
    }
    Some(bytes)
}

pub(super) fn open_artifact(
    root: &Path,
    generation_path: &Path,
    relative_path: &Path,
    pointer: Option<&ActiveGenerationPointer>,
) -> Result<(File, ArtifactIdentity)> {
    open_artifact_with_alias_authority(
        root,
        generation_path,
        relative_path,
        ManagedAliasAuthority::Publication(pointer),
    )
}

fn open_artifact_with_alias_authority(
    root: &Path,
    generation_path: &Path,
    relative_path: &Path,
    alias_authority: ManagedAliasAuthority<'_>,
) -> Result<(File, ArtifactIdentity)> {
    if relative_path.components().count() != 1 {
        return Err(IndexError::ChecksumMismatch);
    }
    let path = relative_path
        .to_str()
        .ok_or(IndexError::ChecksumMismatch)?
        .to_owned();
    let artifact_path = generation_path.join(relative_path);
    let mut unaccounted_observation: Option<(FileIdentity, u64)> = None;
    let mut stable_unaccounted_attempts = 0_usize;
    for _ in 0..ARTIFACT_STABLE_SNAPSHOT_ATTEMPTS {
        let Some((file, identity)) = open_artifact_file_snapshot(&artifact_path)? else {
            unaccounted_observation = None;
            stable_unaccounted_attempts = 0;
            std::thread::yield_now();
            continue;
        };
        match stable_artifact_link_snapshot_with_alias_authority(
            root,
            &artifact_path,
            relative_path,
            &file,
            &identity,
            alias_authority,
        )? {
            ArtifactLinkSnapshot::Stable(identity) => {
                return Ok((file, ArtifactIdentity { path, identity }));
            }
            ArtifactLinkSnapshot::Retry => {
                unaccounted_observation = None;
                stable_unaccounted_attempts = 0;
            }
            ArtifactLinkSnapshot::Unaccounted { identity, aliases } => {
                let observation = (identity, aliases);
                if unaccounted_observation.as_ref() == Some(&observation) {
                    stable_unaccounted_attempts = stable_unaccounted_attempts
                        .checked_add(1)
                        .ok_or(IndexError::CountOverflow)?;
                } else {
                    unaccounted_observation = Some(observation);
                    stable_unaccounted_attempts = 1;
                }
                if stable_unaccounted_attempts == ARTIFACT_STABLE_SNAPSHOT_ATTEMPTS {
                    return Err(IndexError::ChecksumMismatch);
                }
            }
        }
        std::thread::yield_now();
    }
    Err(IndexError::ConcurrentGenerationChange)
}

pub(crate) fn open_authenticated_artifact(
    root: &Path,
    generation_path: &Path,
    relative_path: &Path,
    pointer: Option<&ActiveGenerationPointer>,
) -> Result<(File, ArtifactIdentity)> {
    open_artifact(root, generation_path, relative_path, pointer)
}

pub(crate) fn recapture_authenticated_artifact(
    root: &Path,
    generation_path: &Path,
    relative_path: &Path,
    file: &File,
    pointer: Option<&ActiveGenerationPointer>,
) -> Result<ArtifactIdentity> {
    if relative_path.components().count() != 1 {
        return Err(IndexError::ChecksumMismatch);
    }
    let path = relative_path
        .to_str()
        .ok_or(IndexError::ChecksumMismatch)?
        .to_owned();
    let artifact_path = generation_path.join(relative_path);
    let mut unaccounted_observation: Option<(FileIdentity, u64)> = None;
    let mut stable_unaccounted_attempts = 0_usize;
    for _ in 0..ARTIFACT_STABLE_SNAPSHOT_ATTEMPTS {
        let current = file_identity(file).map_err(|_| IndexError::ChecksumMismatch)?;
        match stable_artifact_link_snapshot(
            root,
            &artifact_path,
            relative_path,
            file,
            &current,
            pointer,
        )? {
            ArtifactLinkSnapshot::Stable(identity) => {
                return Ok(ArtifactIdentity { path, identity });
            }
            ArtifactLinkSnapshot::Retry => {
                unaccounted_observation = None;
                stable_unaccounted_attempts = 0;
            }
            ArtifactLinkSnapshot::Unaccounted { identity, aliases } => {
                let observation = (identity, aliases);
                if unaccounted_observation.as_ref() == Some(&observation) {
                    stable_unaccounted_attempts = stable_unaccounted_attempts
                        .checked_add(1)
                        .ok_or(IndexError::CountOverflow)?;
                } else {
                    unaccounted_observation = Some(observation);
                    stable_unaccounted_attempts = 1;
                }
                if stable_unaccounted_attempts == ARTIFACT_STABLE_SNAPSHOT_ATTEMPTS {
                    return Err(IndexError::ChecksumMismatch);
                }
            }
        }
        std::thread::yield_now();
    }
    Err(IndexError::ConcurrentGenerationChange)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ArtifactLinkSnapshot {
    Stable(FileIdentity),
    Retry,
    Unaccounted {
        identity: FileIdentity,
        aliases: u64,
    },
}

/// Opens a named artifact while distinguishing hard-link topology churn from
/// replacement or payload mutation. `None` asks the bounded caller to retry a
/// snapshot changed only by a link/unlink operation.
fn open_artifact_file_snapshot(path: &Path) -> Result<Option<(File, FileIdentity)>> {
    validate_named_regular_file(path)?;
    let file = open_nofollow(path).map_err(|_| IndexError::ChecksumMismatch)?;
    let opened = file_identity(&file).map_err(|_| IndexError::ChecksumMismatch)?;
    validate_named_regular_file(path)?;
    let named = open_nofollow(path).map_err(|_| IndexError::ChecksumMismatch)?;
    let named_identity = file_identity(&named).map_err(|_| IndexError::ChecksumMismatch)?;
    drop(named);
    let held = file_identity(&file).map_err(|_| IndexError::ChecksumMismatch)?;
    if opened == named_identity && named_identity == held {
        return Ok(Some((file, held)));
    }
    if opened.same_payload_identity(&named_identity) && named_identity.same_payload_identity(&held)
    {
        return Ok(None);
    }
    Err(IndexError::ChecksumMismatch)
}

/// Proves one stable managed-alias snapshot for an already-bound artifact.
/// Stable unaccounted hardlinks remain corruption; only an observation that
/// changed during the bounded snapshot is retryable.
fn stable_artifact_link_snapshot(
    root: &Path,
    artifact_path: &Path,
    relative_path: &Path,
    file: &File,
    identity: &FileIdentity,
    pointer: Option<&ActiveGenerationPointer>,
) -> Result<ArtifactLinkSnapshot> {
    stable_artifact_link_snapshot_with_alias_authority(
        root,
        artifact_path,
        relative_path,
        file,
        identity,
        ManagedAliasAuthority::Publication(pointer),
    )
}

fn stable_artifact_link_snapshot_with_alias_authority(
    root: &Path,
    artifact_path: &Path,
    relative_path: &Path,
    file: &File,
    identity: &FileIdentity,
    alias_authority: ManagedAliasAuthority<'_>,
) -> Result<ArtifactLinkSnapshot> {
    let before = file_identity(file).map_err(|_| IndexError::ChecksumMismatch)?;
    if before != *identity {
        return if before.same_payload_identity(identity) {
            Ok(ArtifactLinkSnapshot::Retry)
        } else {
            Err(IndexError::ChecksumMismatch)
        };
    }
    let Some(alias_snapshot) =
        managed_artifact_alias_count(root, relative_path, &before, alias_authority)?
    else {
        return Ok(ArtifactLinkSnapshot::Retry);
    };
    let after_scan = file_identity(file).map_err(|_| IndexError::ChecksumMismatch)?;
    validate_named_regular_file(artifact_path)?;
    let named = open_nofollow(artifact_path).map_err(|_| IndexError::ChecksumMismatch)?;
    let named_identity = file_identity(&named).map_err(|_| IndexError::ChecksumMismatch)?;
    drop(named);
    let final_identity = file_identity(file).map_err(|_| IndexError::ChecksumMismatch)?;

    if before == after_scan && after_scan == named_identity && named_identity == final_identity {
        if alias_authority.requires_accounted_aliases() && alias_snapshot.unaccounted_aliases != 0 {
            return Ok(ArtifactLinkSnapshot::Unaccounted {
                identity: final_identity,
                aliases: alias_snapshot
                    .aliases
                    .saturating_sub(alias_snapshot.unaccounted_aliases),
            });
        }
        if alias_snapshot.aliases == 0 || alias_snapshot.aliases != final_identity.link_count() {
            if alias_snapshot.saw_unpublished_generation {
                return Ok(ArtifactLinkSnapshot::Retry);
            }
            return Ok(ArtifactLinkSnapshot::Unaccounted {
                identity: final_identity,
                aliases: alias_snapshot.aliases,
            });
        }
        return Ok(ArtifactLinkSnapshot::Stable(final_identity));
    }
    if before.same_payload_identity(&after_scan)
        && after_scan.same_payload_identity(&named_identity)
        && named_identity.same_payload_identity(&final_identity)
    {
        return Ok(ArtifactLinkSnapshot::Retry);
    }
    Err(IndexError::ChecksumMismatch)
}

pub(super) fn recapture_artifact(
    root: &Path,
    generation_path: &Path,
    relative_path: &Path,
    pointer: Option<&ActiveGenerationPointer>,
) -> Result<ArtifactIdentity> {
    capture_artifact(root, generation_path, relative_path, pointer)
}

pub(crate) fn capture_artifact_identity(
    root: &Path,
    generation_path: &Path,
    relative_path: &Path,
    pointer: Option<&ActiveGenerationPointer>,
) -> Result<ArtifactIdentity> {
    capture_artifact(root, generation_path, relative_path, pointer)
}

fn capture_artifact(
    root: &Path,
    generation_path: &Path,
    relative_path: &Path,
    pointer: Option<&ActiveGenerationPointer>,
) -> Result<ArtifactIdentity> {
    let (file, artifact) = open_artifact(root, generation_path, relative_path, pointer)?;
    drop(file);
    Ok(artifact)
}

fn capture_artifact_with_retained_aliases(
    root: &Path,
    generation_path: &Path,
    relative_path: &Path,
    retained_alias_directories: &HashSet<String>,
) -> Result<ArtifactIdentity> {
    let (file, artifact) = open_artifact_with_alias_authority(
        root,
        generation_path,
        relative_path,
        ManagedAliasAuthority::Retained(retained_alias_directories),
    )?;
    drop(file);
    Ok(artifact)
}

fn capture_single_link_control(path: &Path) -> Result<FileIdentity> {
    let (file, identity) = open_regular_file(path)?;
    drop(file);
    if identity.link_count() != 1 {
        return Err(IndexError::ChecksumMismatch);
    }
    Ok(identity)
}

fn capture_pointer_bound_single_link_control(
    root: &Path,
    pointer: &ActiveGenerationPointer,
    path: &Path,
) -> Result<FileIdentity> {
    for attempt in 0..ARTIFACT_STABLE_SNAPSHOT_ATTEMPTS {
        match capture_single_link_control(path) {
            Ok(identity) => {
                if load_current_pointer(root)? != *pointer {
                    return Err(IndexError::ConcurrentGenerationChange);
                }
                return Ok(identity);
            }
            Err(error) => {
                if load_current_pointer(root)? != *pointer {
                    return Err(IndexError::ConcurrentGenerationChange);
                }
                if attempt + 1 == ARTIFACT_STABLE_SNAPSHOT_ATTEMPTS {
                    return Err(error);
                }
                std::thread::yield_now();
            }
        }
    }
    Err(IndexError::ConcurrentGenerationChange)
}

fn open_regular_file(path: &Path) -> Result<(File, FileIdentity)> {
    validate_named_regular_file(path)?;
    let file = open_nofollow(path).map_err(|_| IndexError::ChecksumMismatch)?;
    let identity = file_identity(&file).map_err(|_| IndexError::ChecksumMismatch)?;
    #[cfg(test)]
    run_regular_file_identity_test_hook(path);
    validate_named_regular_file(path)?;
    let named = open_nofollow(path).map_err(|_| IndexError::ChecksumMismatch)?;
    let named_identity = file_identity(&named).map_err(|_| IndexError::ChecksumMismatch)?;
    if identity != named_identity {
        return Err(IndexError::ChecksumMismatch);
    }
    Ok((file, identity))
}

#[cfg(test)]
type PathTestHook = Box<dyn FnMut(&Path)>;

#[cfg(test)]
thread_local! {
    static REGULAR_FILE_IDENTITY_TEST_HOOK: std::cell::RefCell<Option<PathTestHook>> =
        std::cell::RefCell::new(None);
}

#[cfg(test)]
struct RegularFileIdentityTestHookGuard(Option<PathTestHook>);

#[cfg(test)]
impl RegularFileIdentityTestHookGuard {
    fn install(hook: impl FnMut(&Path) + 'static) -> Self {
        let previous =
            REGULAR_FILE_IDENTITY_TEST_HOOK.with(|active| active.replace(Some(Box::new(hook))));
        Self(previous)
    }
}

#[cfg(test)]
impl Drop for RegularFileIdentityTestHookGuard {
    fn drop(&mut self) {
        REGULAR_FILE_IDENTITY_TEST_HOOK.with(|active| active.replace(self.0.take()));
    }
}

#[cfg(test)]
fn run_regular_file_identity_test_hook(path: &Path) {
    REGULAR_FILE_IDENTITY_TEST_HOOK.with(|active| {
        if let Some(hook) = active.borrow_mut().as_mut() {
            hook(path);
        }
    });
}

fn open_nofollow(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        };
        options
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    options.open(path)
}

fn validate_named_regular_file(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path).map_err(|_| IndexError::ChecksumMismatch)?;
    if metadata.file_type().is_symlink()
        || metadata_is_reparse_point(&metadata)
        || !metadata.file_type().is_file()
    {
        return Err(IndexError::ChecksumMismatch);
    }
    Ok(())
}

fn ensure_real_directory(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path).map_err(|_| IndexError::ChecksumMismatch)?;
    if metadata.file_type().is_symlink()
        || metadata_is_reparse_point(&metadata)
        || !metadata.file_type().is_dir()
    {
        return Err(IndexError::ChecksumMismatch);
    }
    Ok(())
}

#[cfg(unix)]
fn file_identity(file: &File) -> std::io::Result<FileIdentity> {
    use std::os::unix::fs::MetadataExt as _;

    let metadata = file.metadata()?;
    if !metadata.file_type().is_file() {
        return Err(std::io::Error::other(
            "index artifact is not a regular file",
        ));
    }
    Ok(FileIdentity {
        length: metadata.len(),
        device: metadata.dev(),
        inode: metadata.ino(),
        mode: metadata.mode(),
        modified_seconds: metadata.mtime(),
        modified_nanoseconds: metadata.mtime_nsec(),
        changed_seconds: metadata.ctime(),
        changed_nanoseconds: metadata.ctime_nsec(),
        links: metadata.nlink(),
    })
}

#[cfg(windows)]
fn file_identity(file: &File) -> std::io::Result<FileIdentity> {
    use std::{mem::size_of, os::windows::io::AsRawHandle as _};
    use windows_sys::Win32::{
        Foundation::HANDLE,
        Storage::FileSystem::{
            FileBasicInfo, FileIdInfo, GetFileInformationByHandle, GetFileInformationByHandleEx,
            BY_HANDLE_FILE_INFORMATION, FILE_BASIC_INFO, FILE_ID_INFO,
        },
    };

    let handle = file.as_raw_handle() as HANDLE;
    let mut basic = FILE_BASIC_INFO::default();
    if unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileBasicInfo,
            (&mut basic as *mut FILE_BASIC_INFO).cast(),
            size_of::<FILE_BASIC_INFO>() as u32,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error());
    }
    let mut id = FILE_ID_INFO::default();
    if unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileIdInfo,
            (&mut id as *mut FILE_ID_INFO).cast(),
            size_of::<FILE_ID_INFO>() as u32,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error());
    }
    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    if unsafe { GetFileInformationByHandle(handle, &mut information) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(FileIdentity {
        length: file.metadata()?.len(),
        volume_serial_number: id.VolumeSerialNumber,
        file_id: id.FileId.Identifier,
        creation_time: basic.CreationTime,
        last_write_time: basic.LastWriteTime,
        change_time: basic.ChangeTime,
        attributes: basic.FileAttributes,
        links: information.nNumberOfLinks,
    })
}

#[cfg(not(any(unix, windows)))]
fn file_identity(_file: &File) -> std::io::Result<FileIdentity> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "strong index artifact identity is unsupported on this platform",
    ))
}

#[cfg(windows)]
fn metadata_is_reparse_point(metadata: &Metadata) -> bool {
    use std::os::windows::fs::MetadataExt as _;
    use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn metadata_is_reparse_point(_metadata: &Metadata) -> bool {
    false
}

#[derive(Clone, Copy)]
enum ManagedAliasAuthority<'a> {
    Publication(Option<&'a ActiveGenerationPointer>),
    Retained(&'a HashSet<String>),
}

impl ManagedAliasAuthority<'_> {
    fn accounts_directory(&self, directory_name: &str) -> bool {
        match self {
            Self::Publication(None) => true,
            Self::Publication(Some(pointer)) => {
                pointer.active().directory() == directory_name
                    || pointer
                        .previous()
                        .is_some_and(|slot| slot.directory() == directory_name)
            }
            Self::Retained(directories) => directories.contains(directory_name),
        }
    }

    fn tracks_unpublished_generations(&self) -> bool {
        matches!(self, Self::Publication(Some(_)))
    }

    fn requires_accounted_aliases(&self) -> bool {
        matches!(self, Self::Retained(_))
    }
}

fn managed_artifact_alias_count(
    root: &Path,
    relative_path: &Path,
    identity: &FileIdentity,
    alias_authority: ManagedAliasAuthority<'_>,
) -> Result<Option<ManagedAliasSnapshot>> {
    let generations = root.join(INDEX_GENERATIONS_DIRECTORY);
    let mut aliases = 0_u64;
    let mut unaccounted_aliases = 0_u64;
    let mut saw_unpublished_generation = false;
    for entry in fs::read_dir(generations).map_err(|_| IndexError::ChecksumMismatch)? {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) if retryable_alias_snapshot_error(&error) => return Ok(None),
            Err(_) => return Err(IndexError::ChecksumMismatch),
        };
        #[cfg(test)]
        run_alias_entry_test_hook(&entry.path());
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(error) if retryable_alias_snapshot_error(&error) => return Ok(None),
            Err(_) => return Err(IndexError::ChecksumMismatch),
        };
        let Some(directory_name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !file_type.is_dir() || !is_generation_directory_name(&directory_name) {
            continue;
        }
        let accounted_directory = alias_authority.accounts_directory(&directory_name);
        if alias_authority.tracks_unpublished_generations() && !accounted_directory {
            saw_unpublished_generation = true;
        }
        let candidate = entry.path().join(relative_path);
        let (file, candidate_identity) = match open_regular_file(&candidate) {
            Ok(opened) => opened,
            Err(_) => continue,
        };
        drop(file);
        if candidate_identity.same_native_file(identity) {
            aliases = aliases.checked_add(1).ok_or(IndexError::CountOverflow)?;
            if !accounted_directory {
                unaccounted_aliases = unaccounted_aliases
                    .checked_add(1)
                    .ok_or(IndexError::CountOverflow)?;
            }
        }
    }
    Ok(Some(ManagedAliasSnapshot {
        aliases,
        unaccounted_aliases,
        saw_unpublished_generation,
    }))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ManagedAliasSnapshot {
    aliases: u64,
    unaccounted_aliases: u64,
    saw_unpublished_generation: bool,
}

fn retryable_alias_snapshot_error(error: &std::io::Error) -> bool {
    if error.kind() == std::io::ErrorKind::NotFound {
        return true;
    }
    #[cfg(unix)]
    {
        error.raw_os_error() == Some(libc::ESTALE)
    }
    #[cfg(not(unix))]
    {
        false
    }
}

#[cfg(test)]
thread_local! {
    static ALIAS_ENTRY_TEST_HOOK: std::cell::RefCell<Option<PathTestHook>> =
        std::cell::RefCell::new(None);
}

#[cfg(test)]
struct AliasEntryTestHookGuard(Option<PathTestHook>);

#[cfg(test)]
impl AliasEntryTestHookGuard {
    fn install(hook: impl FnMut(&Path) + 'static) -> Self {
        let previous = ALIAS_ENTRY_TEST_HOOK.with(|active| active.replace(Some(Box::new(hook))));
        Self(previous)
    }
}

#[cfg(test)]
impl Drop for AliasEntryTestHookGuard {
    fn drop(&mut self) {
        ALIAS_ENTRY_TEST_HOOK.with(|active| active.replace(self.0.take()));
    }
}

#[cfg(test)]
fn run_alias_entry_test_hook(path: &Path) {
    ALIAS_ENTRY_TEST_HOOK.with(|active| {
        if let Some(hook) = active.borrow_mut().as_mut() {
            hook(path);
        }
    });
}

fn is_generation_directory_name(name: &str) -> bool {
    name.strip_prefix("generation-").is_some_and(|suffix| {
        suffix.len() == 32
            && suffix
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn certification_file_name(slot: &GenerationSlot) -> String {
    format!("{}{CERTIFICATION_SUFFIX}", slot.directory())
}

pub fn certification_path(root: &Path, slot: &GenerationSlot) -> PathBuf {
    root.join(CERTIFICATION_DIRECTORY)
        .join(certification_file_name(slot))
}

pub fn reclaim_unreferenced_certifications(
    root: &Path,
    pointer: Option<&ActiveGenerationPointer>,
    lease: Option<&GenerationRetentionLease>,
) -> Result<()> {
    ensure_generation_read_lease_coordinator(root)?;
    let directory = root.join(CERTIFICATION_DIRECTORY);
    fs::create_dir_all(&directory)?;
    let retained = pointer
        .into_iter()
        .flat_map(|pointer| std::iter::once(pointer.active()).chain(pointer.previous()))
        .map(GenerationSlot::directory)
        .chain(lease.map(|lease| lease.target().directory()))
        .map(str::to_owned)
        .collect::<std::collections::HashSet<_>>();
    let mut removed = false;
    for entry in fs::read_dir(&directory)? {
        let entry = entry?;
        let Some(file_name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Some(generation_directory) = file_name.strip_suffix(CERTIFICATION_SUFFIX) else {
            continue;
        };
        if is_generation_directory_name(generation_directory)
            && !retained.contains(generation_directory)
        {
            let Some(_reclaim_authority) =
                try_generation_directory_reclaim_authority(root, generation_directory)?
            else {
                continue;
            };
            fs::remove_file(entry.path())?;
            removed = true;
        }
    }
    if removed {
        sync_directory(&directory)?;
    }
    Ok(())
}

#[cfg(any(test, feature = "test-support"))]
pub fn certification_file_for_active(root: &Path) -> Result<PathBuf> {
    let pointer = load_current_pointer(root)?;
    Ok(certification_path(root, pointer.active()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    struct ReadOnlyCertificationFixture {
        temp: tempfile::TempDir,
        slot: GenerationSlot,
        index: tantivy::Index,
        relative_artifact_path: PathBuf,
        certified_artifact: ArtifactIdentity,
    }

    #[cfg(unix)]
    impl ReadOnlyCertificationFixture {
        fn root(&self) -> &Path {
            self.temp.path()
        }

        fn artifact_path(&self) -> PathBuf {
            slot_path(self.root(), &self.slot).join(&self.relative_artifact_path)
        }
    }

    #[cfg(unix)]
    fn read_only_certification_fixture() -> ReadOnlyCertificationFixture {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let mut schema = tantivy::schema::Schema::builder();
        let body = schema.add_text_field("body", tantivy::schema::TEXT | tantivy::schema::STORED);
        let candidate =
            crate::create_candidate_generation(root, None, schema.build(), 50_000_000).unwrap();
        let directory_name = candidate.directory_name.clone();
        let index = candidate.index;
        let mut writer = index.writer(50_000_000).unwrap();
        writer
            .add_document(tantivy::doc!(body => "immutable payload"))
            .unwrap();
        writer.commit().unwrap();
        writer.wait_merging_threads().unwrap();

        let generation_path = root.join(INDEX_GENERATIONS_DIRECTORY).join(&directory_name);
        let audit = physical_integrity_audit(&index, &generation_path, None).unwrap();
        let slot =
            GenerationSlot::new("1".repeat(64), directory_name, audit.digest().to_owned()).unwrap();
        let pointer = ActiveGenerationPointer::new(slot.clone(), None).unwrap();
        fs::create_dir_all(root.join(MANIFEST_DIRECTORY)).unwrap();
        fs::write(manifest_path(root, slot.generation_id()), b"manifest").unwrap();
        crate::publish_active_generation_pointer(root, &pointer).unwrap();
        let certified =
            install_certification(root, &pointer, &slot, &index, &audit, false).unwrap();
        let relative_artifact_path = active_index_files(&index)
            .unwrap()
            .into_iter()
            .find(|path| {
                fs::metadata(generation_path.join(path)).is_ok_and(|metadata| metadata.len() > 0)
            })
            .unwrap();
        let (certified_artifact, _, sealed) = certified
            .certified_artifact(&relative_artifact_path)
            .unwrap();
        assert!(sealed);
        assert!(certified_artifact.identity.is_readonly());

        ReadOnlyCertificationFixture {
            temp,
            slot,
            index,
            relative_artifact_path,
            certified_artifact,
        }
    }

    #[cfg(unix)]
    fn mutate_same_length_and_restore_metadata(path: &Path) -> (Metadata, Metadata) {
        use std::{io::Write as _, os::unix::fs::PermissionsExt as _};

        let before = fs::metadata(path).unwrap();
        let original_permissions = before.permissions();
        let modified = before.modified().unwrap();
        let mut bytes = fs::read(path).unwrap();
        bytes[0] ^= 0x5a;

        let mut writable = original_permissions.clone();
        writable.set_mode(writable.mode() | 0o200);
        fs::set_permissions(path, writable).unwrap();
        let mut file = OpenOptions::new().write(true).open(path).unwrap();
        file.write_all(&bytes).unwrap();
        file.set_times(std::fs::FileTimes::new().set_modified(modified))
            .unwrap();
        file.sync_all().unwrap();
        drop(file);
        fs::set_permissions(path, original_permissions).unwrap();

        (before, fs::metadata(path).unwrap())
    }

    fn generation(root: &Path, digit: char) -> PathBuf {
        root.join(INDEX_GENERATIONS_DIRECTORY)
            .join(format!("generation-{}", digit.to_string().repeat(32)))
    }

    fn pointer(digit: char) -> ActiveGenerationPointer {
        let digit = digit.to_string();
        ActiveGenerationPointer::new(
            GenerationSlot::new(
                digit.repeat(64),
                format!("generation-{}", digit.repeat(32)),
                digit.repeat(64),
            )
            .unwrap(),
            None,
        )
        .unwrap()
    }

    #[test]
    fn managed_link_creation_and_cleanup_are_retryable_stable_snapshots() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let active = generation(root, '1');
        let candidate = generation(root, '2');
        fs::create_dir_all(&active).unwrap();
        fs::create_dir_all(&candidate).unwrap();
        let relative = Path::new("payload.bin");
        let active_path = active.join(relative);
        let candidate_path = candidate.join(relative);
        fs::write(&active_path, b"immutable payload").unwrap();

        let (file, before_link) = open_artifact_file_snapshot(&active_path).unwrap().unwrap();
        fs::hard_link(&active_path, &candidate_path).unwrap();
        assert!(matches!(
            stable_artifact_link_snapshot(root, &active_path, relative, &file, &before_link, None,)
                .unwrap(),
            ArtifactLinkSnapshot::Retry
        ));
        drop(file);

        let (_, linked) = open_artifact(root, &active, relative, None).unwrap();
        assert_eq!(linked.identity.link_count(), 2);
        let (file, before_unlink) = open_artifact_file_snapshot(&active_path).unwrap().unwrap();
        fs::remove_file(&candidate_path).unwrap();
        assert!(matches!(
            stable_artifact_link_snapshot(
                root,
                &active_path,
                relative,
                &file,
                &before_unlink,
                None,
            )
            .unwrap(),
            ArtifactLinkSnapshot::Retry
        ));
        drop(file);

        let (_, unlinked) = open_artifact(root, &active, relative, None).unwrap();
        assert_eq!(unlinked.identity.link_count(), 1);
        assert!(linked.same_payload_identity_changed(&unlinked));
    }

    #[cfg(unix)]
    #[test]
    fn activation_certification_rejects_mutation_masked_by_link_reclamation() {
        let fixture = read_only_certification_fixture();
        let root = fixture.root();
        let generation_path = slot_path(root, &fixture.slot);
        let artifact_path = fixture.artifact_path();
        let candidate_generation = generation(root, 'f');
        let candidate_artifact = candidate_generation.join(&fixture.relative_artifact_path);
        fs::create_dir_all(candidate_artifact.parent().unwrap()).unwrap();
        fs::hard_link(&artifact_path, &candidate_artifact).unwrap();
        let pointer = load_current_pointer(root).unwrap();
        let audit =
            physical_integrity_audit(&fixture.index, &generation_path, Some(&pointer)).unwrap();

        let certified_bytes = fs::read(&artifact_path).unwrap();
        mutate_same_length_and_restore_metadata(&artifact_path);
        assert_ne!(fs::read(&artifact_path).unwrap(), certified_bytes);
        fs::remove_file(&candidate_artifact).unwrap();
        fs::remove_dir(&candidate_generation).unwrap();

        crate::reset_physical_verification_activity();
        assert!(matches!(
            certify_activated_generation(root, &pointer, &fixture.slot, &fixture.index, &audit,),
            Err(IndexError::ConcurrentGenerationChange)
        ));
        assert_eq!(crate::checksum_walks(), 0);
        assert_eq!(crate::hashed_artifact_bytes(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn read_only_certification_rejects_restored_metadata_byte_mutation() {
        use std::os::unix::fs::MetadataExt as _;

        let fixture = read_only_certification_fixture();
        let artifact_path = fixture.artifact_path();
        let (before, after) = mutate_same_length_and_restore_metadata(&artifact_path);
        assert_eq!(after.len(), before.len());
        assert_eq!(after.modified().unwrap(), before.modified().unwrap());
        assert_eq!(after.mode(), before.mode());
        assert!(after.permissions().readonly());
        assert_eq!(after.nlink(), before.nlink());

        crate::reset_physical_verification_activity();
        assert!(matches!(
            verify_physical_integrity_read_only(fixture.root(), &fixture.slot, &fixture.index),
            Err(IndexError::ChecksumMismatch)
        ));
        assert_eq!(crate::checksum_walks(), 0);
        assert_eq!(crate::hashed_artifact_bytes(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn read_only_certification_rejects_unretained_alias_and_restored_metadata_mutation() {
        use std::os::unix::fs::MetadataExt as _;

        let fixture = read_only_certification_fixture();
        let artifact_path = fixture.artifact_path();
        let attacker_generation = generation(fixture.root(), 'd');
        let external_alias = attacker_generation.join(&fixture.relative_artifact_path);
        fs::create_dir_all(external_alias.parent().unwrap()).unwrap();
        fs::hard_link(&artifact_path, &external_alias).unwrap();
        assert_eq!(
            fs::metadata(&artifact_path).unwrap().nlink(),
            fixture.certified_artifact.identity.link_count() + 1
        );

        let (before, after) = mutate_same_length_and_restore_metadata(&artifact_path);
        assert_eq!(after.len(), before.len());
        assert_eq!(after.modified().unwrap(), before.modified().unwrap());
        assert_eq!(after.mode(), before.mode());
        assert!(after.permissions().readonly());
        assert_eq!(after.nlink(), before.nlink());

        crate::reset_physical_verification_activity();
        assert!(matches!(
            verify_physical_integrity_read_only(fixture.root(), &fixture.slot, &fixture.index),
            Err(IndexError::ChecksumMismatch)
        ));
        assert_eq!(crate::checksum_walks(), 0);
        assert_eq!(crate::hashed_artifact_bytes(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn read_only_certification_rejects_accounted_link_transition_without_hashing() {
        use std::os::unix::fs::MetadataExt as _;

        let fixture = read_only_certification_fixture();
        let artifact_path = fixture.artifact_path();
        let linked_generation = generation(fixture.root(), 'e');
        let linked_artifact = linked_generation.join(&fixture.relative_artifact_path);
        fs::create_dir_all(linked_artifact.parent().unwrap()).unwrap();
        fs::hard_link(&artifact_path, &linked_artifact).unwrap();
        let linked_slot = GenerationSlot::new(
            "e".repeat(64),
            format!("generation-{}", "e".repeat(32)),
            "e".repeat(64),
        )
        .unwrap();
        let pointer =
            ActiveGenerationPointer::new(linked_slot, Some(fixture.slot.clone())).unwrap();
        crate::publish_active_generation_pointer(fixture.root(), &pointer).unwrap();

        let linked = fs::metadata(&artifact_path).unwrap();
        assert_eq!(
            linked.nlink(),
            fixture.certified_artifact.identity.link_count() + 1
        );
        crate::reset_physical_verification_activity();
        assert!(matches!(
            verify_physical_integrity_read_only(fixture.root(), &fixture.slot, &fixture.index),
            Err(IndexError::ChecksumMismatch)
        ));
        assert_eq!(crate::checksum_walks(), 0);
        assert_eq!(crate::hashed_artifact_bytes(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn read_only_certification_rejects_mutation_masked_by_accounted_link_transition() {
        use std::os::unix::fs::MetadataExt as _;

        let fixture = read_only_certification_fixture();
        let artifact_path = fixture.artifact_path();
        let certified_bytes = fs::read(&artifact_path).unwrap();
        let (before_mutation, after_mutation) =
            mutate_same_length_and_restore_metadata(&artifact_path);
        assert_ne!(fs::read(&artifact_path).unwrap(), certified_bytes);
        assert_eq!(after_mutation.len(), before_mutation.len());
        assert_eq!(
            after_mutation.modified().unwrap(),
            before_mutation.modified().unwrap()
        );
        assert_eq!(after_mutation.mode(), before_mutation.mode());
        assert_eq!(after_mutation.nlink(), before_mutation.nlink());

        let linked_generation = generation(fixture.root(), 'e');
        let linked_artifact = linked_generation.join(&fixture.relative_artifact_path);
        fs::create_dir_all(linked_artifact.parent().unwrap()).unwrap();
        fs::hard_link(&artifact_path, &linked_artifact).unwrap();
        let linked_slot = GenerationSlot::new(
            "e".repeat(64),
            format!("generation-{}", "e".repeat(32)),
            "e".repeat(64),
        )
        .unwrap();
        let pointer =
            ActiveGenerationPointer::new(linked_slot, Some(fixture.slot.clone())).unwrap();
        crate::publish_active_generation_pointer(fixture.root(), &pointer).unwrap();

        let linked = fs::metadata(&artifact_path).unwrap();
        assert_eq!(
            linked.nlink(),
            fixture.certified_artifact.identity.link_count() + 1
        );
        crate::reset_physical_verification_activity();
        assert!(matches!(
            verify_physical_integrity_read_only(fixture.root(), &fixture.slot, &fixture.index),
            Err(IndexError::ChecksumMismatch)
        ));
        assert_eq!(crate::checksum_walks(), 0);
        assert_eq!(crate::hashed_artifact_bytes(), 0);
    }

    #[test]
    fn generation_disappearing_during_alias_scan_is_retryable() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let active = generation(root, '1');
        let candidate = generation(root, '2');
        fs::create_dir_all(&active).unwrap();
        fs::create_dir_all(&candidate).unwrap();
        let relative = Path::new("payload.bin");
        let active_path = active.join(relative);
        let candidate_path = candidate.join(relative);
        fs::write(&active_path, b"immutable payload").unwrap();
        fs::hard_link(&active_path, &candidate_path).unwrap();
        let (file, linked) = open_artifact_file_snapshot(&active_path).unwrap().unwrap();

        let candidate_for_hook = candidate.clone();
        let candidate_path_for_hook = candidate_path.clone();
        let _hook = AliasEntryTestHookGuard::install(move |entry_path| {
            if entry_path == candidate_for_hook {
                fs::remove_file(&candidate_path_for_hook).unwrap();
                fs::remove_dir(&candidate_for_hook).unwrap();
            }
        });

        assert!(matches!(
            stable_artifact_link_snapshot(root, &active_path, relative, &file, &linked, None,)
                .unwrap(),
            ArtifactLinkSnapshot::Retry
        ));
    }

    #[test]
    fn stale_directory_entry_errors_are_retryable_but_io_errors_are_not() {
        assert!(retryable_alias_snapshot_error(&std::io::Error::from(
            std::io::ErrorKind::NotFound,
        )));
        assert!(!retryable_alias_snapshot_error(&std::io::Error::from(
            std::io::ErrorKind::PermissionDenied,
        )));
        #[cfg(unix)]
        assert!(retryable_alias_snapshot_error(
            &std::io::Error::from_raw_os_error(libc::ESTALE)
        ));
    }

    #[test]
    fn pointer_replacement_during_control_capture_is_concurrent_not_corruption() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let first = pointer('1');
        let second = pointer('2');
        let target = root.join("active-generation.json");
        fs::write(&target, serde_json::to_vec(&first).unwrap()).unwrap();
        let directory = DurableMmapDirectory::open(root).unwrap();
        let target_for_hook = target.clone();
        let second_bytes = serde_json::to_vec(&second).unwrap();
        let mut replaced = false;
        let hook = RegularFileIdentityTestHookGuard::install(move |path| {
            if path == target_for_hook && !replaced {
                directory
                    .atomic_write(Path::new("active-generation.json"), &second_bytes)
                    .unwrap();
                replaced = true;
            }
        });

        assert!(matches!(
            capture_pointer_bound_single_link_control(root, &first, &target),
            Err(IndexError::ConcurrentGenerationChange)
        ));
        drop(hook);
        assert_eq!(load_current_pointer(root).unwrap(), second);

        let directory = DurableMmapDirectory::open(root).unwrap();
        let target_for_hook = target.clone();
        let second_bytes = serde_json::to_vec(&second).unwrap();
        let mut rewritten = false;
        let hook = RegularFileIdentityTestHookGuard::install(move |path| {
            if path == target_for_hook && !rewritten {
                directory
                    .atomic_write(Path::new("active-generation.json"), &second_bytes)
                    .unwrap();
                rewritten = true;
            }
        });
        assert!(capture_pointer_bound_single_link_control(root, &second, &target).is_ok());
        drop(hook);

        fs::hard_link(&target, root.join("unmanaged-pointer-hardlink")).unwrap();
        assert!(matches!(
            capture_pointer_bound_single_link_control(root, &second, &target),
            Err(IndexError::ChecksumMismatch)
        ));
    }

    #[test]
    fn stable_unmanaged_hardlink_remains_checksum_mismatch() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let active = generation(root, '1');
        fs::create_dir_all(&active).unwrap();
        let relative = Path::new("payload.bin");
        let active_path = active.join(relative);
        fs::write(&active_path, b"immutable payload").unwrap();
        fs::hard_link(&active_path, root.join("unmanaged-hardlink")).unwrap();

        assert!(matches!(
            open_artifact(root, &active, relative, None),
            Err(IndexError::ChecksumMismatch)
        ));
    }
}
