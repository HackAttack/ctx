use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{File, Metadata, Permissions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    time::SystemTime,
};

use sha2::{Digest, Sha256};
use tantivy::Index;
use uuid::Uuid;

use super::{
    admit_clone_resource, record_candidate_clone_metrics, validate_single_component,
    CandidateActivationFence, CandidateCloneMetrics, MANAGED_FILE, MAX_REPUBLISH_CLONE_BYTES,
    MAX_REPUBLISH_CLONE_FILES, MAX_REPUBLISH_DIRECTORY_ENTRIES, REPUBLISH_HEADROOM_RESERVE_BYTES,
    TANTIVY_LOCK_FILES,
};
use crate::{
    active_index_files,
    certification::{
        capture_artifact_identity, open_authenticated_artifact, recapture_authenticated_artifact,
    },
    lexical_index_settings,
    physical::{PhysicalFileDigest, MAX_MANAGED_METADATA_BYTES},
    physical_integrity_digest, verify_or_certify_physical_integrity, ActiveGenerationPointer,
    CandidateGeneration, CandidatePhysicalProof, CertifiedPhysicalIntegrity, DurableMmapDirectory,
    GenerationError as IndexError, Result, INDEX_GENERATIONS_DIRECTORY,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryKind {
    Regular,
    Directory,
    LinkOrReparse,
    Special,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ObjectIdentity {
    first: u64,
    second: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileIdentity {
    object: ObjectIdentity,
    bytes: u64,
    modified: Option<SystemTime>,
    permissions: PermissionIdentity,
}

impl FileIdentity {
    fn from_file(file: &File) -> Result<Self> {
        let metadata = file.metadata()?;
        require_regular(entry_kind(&metadata)?)?;
        Ok(Self {
            object: platform::object_identity(file)?,
            bytes: metadata.len(),
            modified: metadata.modified().ok(),
            permissions: platform::permission_identity(&metadata),
        })
    }
}

#[cfg(unix)]
type PermissionIdentity = u32;

#[cfg(windows)]
type PermissionIdentity = bool;

struct BoundDirectory {
    path: PathBuf,
    file: File,
    identity: ObjectIdentity,
}

impl BoundDirectory {
    fn open_path(path: &Path) -> Result<Self> {
        let file = platform::open_directory_path(path).map_err(source_topology_open_error)?;
        require_directory(entry_kind(&file.metadata()?)?)?;
        let identity = platform::object_identity(&file)?;
        Ok(Self {
            path: path.to_path_buf(),
            file,
            identity,
        })
    }

    fn open_at(parent: &Self, name: &Path) -> Result<Self> {
        validate_single_component(name)?;
        let file = platform::open_directory_at(&parent.file, &parent.path, name)
            .map_err(source_topology_open_error)?;
        Self::from_child(parent, name, file)
    }

    fn create_at(parent: &Self, name: &Path) -> Result<Self> {
        validate_single_component(name)?;
        let file = platform::create_directory_at(&parent.file, &parent.path, name)?;
        Self::from_child(parent, name, file)
    }

    fn from_child(parent: &Self, name: &Path, file: File) -> Result<Self> {
        require_directory(entry_kind(&file.metadata()?)?)?;
        let identity = platform::object_identity(&file)?;
        let directory = Self {
            path: parent.path.join(name),
            file,
            identity,
        };
        directory.validate_child_binding(parent, name)?;
        Ok(directory)
    }

    fn validate_child_binding(&self, parent: &Self, name: &Path) -> Result<()> {
        let named = platform::open_directory_at(&parent.file, &parent.path, name)
            .map_err(source_topology_open_error)?;
        require_directory(entry_kind(&named.metadata()?)?)?;
        if platform::object_identity(&named)? != self.identity {
            return Err(IndexError::CurrentRepublishSourceTopology(
                "republish directory changed after authentication",
            ));
        }
        Ok(())
    }

    fn validate_path_binding(&self) -> Result<()> {
        let named = Self::open_path(&self.path)?;
        if named.identity != self.identity {
            return Err(IndexError::CurrentRepublishSourceTopology(
                "republish directory path changed after authentication",
            ));
        }
        Ok(())
    }
}

pub(super) struct CandidateGuard {
    _root: BoundDirectory,
    generations: BoundDirectory,
    destination_name: PathBuf,
    destination: BoundDirectory,
}

impl CandidateGuard {
    pub(super) fn bind(root: &Path, destination_name: &Path) -> Result<Self> {
        validate_single_component(destination_name)?;
        let root = BoundDirectory::open_path(root)?;
        let generations = BoundDirectory::open_at(&root, Path::new(INDEX_GENERATIONS_DIRECTORY))?;
        let destination = BoundDirectory::open_at(&generations, destination_name)?;
        Ok(Self {
            _root: root,
            generations,
            destination_name: destination_name.to_path_buf(),
            destination,
        })
    }

    pub(super) fn validate_binding(&self) -> Result<()> {
        self._root.validate_path_binding()?;
        self.generations
            .validate_child_binding(&self._root, Path::new(INDEX_GENERATIONS_DIRECTORY))?;
        self.destination
            .validate_child_binding(&self.generations, &self.destination_name)
    }

    pub(super) fn discard(self) {
        if clone_checkpoint(PortableCloneStage::BeforeCleanup, &self.destination_name).is_err()
            || self.validate_binding().is_err()
        {
            return;
        }
        if platform::discard_destination(
            &self.generations.file,
            &self.generations.path,
            &self.destination_name,
            &self.destination.file,
            &self.destination.path,
        )
        .is_ok()
        {
            let _ = platform::sync_directory(&self.generations.file);
        }
    }
}

#[derive(Debug, Clone)]
struct PlannedFile {
    path: PathBuf,
    identity: FileIdentity,
    permissions: Permissions,
}

struct ClonePlan {
    files: Vec<PlannedFile>,
    logical_bytes: u64,
    required_headroom: u64,
    managed_bytes: Vec<u8>,
}

impl ClonePlan {
    fn writer_output_headroom(&self, writer_memory_bytes: u64) -> Result<u64> {
        self.logical_bytes
            .checked_add(writer_memory_bytes)
            .and_then(|bytes| bytes.checked_add(REPUBLISH_HEADROOM_RESERVE_BYTES))
            .ok_or(IndexError::CountOverflow)
    }

    fn full_copy_candidate_headroom(&self, writer_memory_bytes: u64) -> Result<u64> {
        self.logical_bytes
            .checked_add(self.writer_output_headroom(writer_memory_bytes)?)
            .ok_or(IndexError::CountOverflow)
    }
}

pub(super) fn create_authenticated_republish_candidate(
    root: &Path,
    predecessor_pointer: &ActiveGenerationPointer,
    predecessor_index: &Index,
) -> Result<CandidateGeneration> {
    let base = predecessor_pointer.active();
    let root_directory = BoundDirectory::open_path(root)?;
    let generations_name = Path::new(INDEX_GENERATIONS_DIRECTORY);
    let generations = BoundDirectory::open_at(&root_directory, generations_name)?;
    let source_name = Path::new(base.directory());
    validate_single_component(source_name)?;
    let source = BoundDirectory::open_at(&generations, source_name)?;

    let plan = authenticated_clone_plan(&generations, source_name, &source, predecessor_index)?;
    let available = available_bytes(&generations, false)?;
    record_plan_metrics(&plan, available);
    if available < plan.required_headroom {
        return Err(IndexError::CurrentRepublishInsufficientHeadroom {
            available,
            required: plan.required_headroom,
        });
    }

    let directory_name = format!("generation-{}", Uuid::now_v7().simple());
    let destination_name = PathBuf::from(&directory_name);
    let destination = BoundDirectory::create_at(&generations, &destination_name)?;
    platform::restrict_destination_directory(&destination.file)?;
    let guard = CandidateGuard {
        _root: root_directory,
        generations,
        destination_name,
        destination,
    };
    let destination_path = guard.destination.path.clone();
    let clone_result = (|| {
        source.validate_child_binding(&guard.generations, source_name)?;
        guard.validate_binding()?;
        clone_files(
            &guard.generations,
            source_name,
            &source,
            &guard.destination_name,
            &guard.destination,
            &plan,
        )?;
        platform::sync_directory(&guard.destination.file)?;
        platform::sync_directory(&guard.generations.file)?;
        source.validate_child_binding(&guard.generations, source_name)?;
        guard.validate_binding()?;

        let directory =
            DurableMmapDirectory::open(&destination_path).map_err(tantivy::TantivyError::from)?;
        let index = Index::open(directory)?;
        if index.settings() != &lexical_index_settings() {
            return Err(IndexError::IndexSettingsMismatch);
        }
        let cloned_digest =
            physical_integrity_digest(&index, &destination_path, Some(predecessor_pointer))?;
        if cloned_digest != base.physical_integrity_digest() {
            return Err(IndexError::ChecksumMismatch);
        }
        Ok((directory_name, index, CandidatePhysicalProof::default()))
    })();
    match clone_result {
        Ok((directory_name, index, physical_proof)) => Ok(CandidateGeneration {
            directory_name,
            index,
            physical_proof,
            activation_fence: CandidateActivationFence::portable(guard),
        }),
        Err(error) => {
            guard.discard();
            Err(error)
        }
    }
}

pub(super) fn create_authenticated_candidate_generation(
    root: &Path,
    predecessor_pointer: &ActiveGenerationPointer,
    predecessor_index: &Index,
    writer_memory_bytes: u64,
) -> Result<CandidateGeneration> {
    let base = predecessor_pointer.active();
    let certified =
        verify_or_certify_physical_integrity(root, predecessor_pointer, base, predecessor_index)?;
    let root_directory = BoundDirectory::open_path(root)?;
    let generations_name = Path::new(INDEX_GENERATIONS_DIRECTORY);
    let generations = BoundDirectory::open_at(&root_directory, generations_name)?;
    let source_name = Path::new(base.directory());
    validate_single_component(source_name)?;
    let source = BoundDirectory::open_at(&generations, source_name)?;

    let plan = authenticated_clone_plan(&generations, source_name, &source, predecessor_index)?;
    let required_headroom = plan.full_copy_candidate_headroom(writer_memory_bytes)?;
    let writer_output_headroom = plan.writer_output_headroom(writer_memory_bytes)?;
    let available = available_bytes(&generations, false)?;
    record_plan_metrics_with_required(&plan, available, required_headroom);
    if available < required_headroom {
        return Err(IndexError::CurrentRepublishInsufficientHeadroom {
            available,
            required: required_headroom,
        });
    }

    let directory_name = format!("generation-{}", Uuid::now_v7().simple());
    let destination_name = PathBuf::from(&directory_name);
    let destination = BoundDirectory::create_at(&generations, &destination_name)?;
    platform::restrict_destination_directory(&destination.file)?;
    let guard = CandidateGuard {
        _root: root_directory,
        generations,
        destination_name,
        destination,
    };
    let source_path = guard.generations.path.join(source_name);
    let destination_path = guard.destination.path.clone();
    let clone_result = (|| {
        source.validate_child_binding(&guard.generations, source_name)?;
        guard.validate_binding()?;
        let mut physical_proof = CandidatePhysicalProof::default();
        let mut metrics = CandidateCloneMetrics::default();
        clone_candidate_files(
            root,
            &source_path,
            predecessor_pointer,
            &certified,
            &guard.generations,
            source_name,
            &source,
            &guard.destination_name,
            &guard.destination,
            &plan,
            writer_output_headroom,
            &mut physical_proof,
            &mut metrics,
        )?;
        platform::sync_directory(&guard.destination.file)?;
        platform::sync_directory(&guard.generations.file)?;
        source.validate_child_binding(&guard.generations, source_name)?;
        guard.validate_binding()?;

        let directory =
            DurableMmapDirectory::open(&destination_path).map_err(tantivy::TantivyError::from)?;
        let index = Index::open(directory)?;
        if index.settings() != &lexical_index_settings() {
            return Err(IndexError::IndexSettingsMismatch);
        }
        record_candidate_clone_metrics(metrics);
        Ok((directory_name, index, physical_proof))
    })();
    match clone_result {
        Ok((directory_name, index, physical_proof)) => Ok(CandidateGeneration {
            directory_name,
            index,
            physical_proof,
            activation_fence: CandidateActivationFence::portable(guard),
        }),
        Err(error) => {
            guard.discard();
            Err(error)
        }
    }
}

fn authenticated_clone_plan(
    generations: &BoundDirectory,
    source_name: &Path,
    source: &BoundDirectory,
    index: &Index,
) -> Result<ClonePlan> {
    let mut active = active_index_files(index)?;
    active.insert(PathBuf::from("meta.json"));
    for path in &active {
        validate_single_component(path)?;
    }

    let mut seen_active = BTreeSet::new();
    let mut managed_seen = false;
    let mut planned = BTreeMap::new();
    let mut total_files = 0_usize;
    let mut total_bytes = 0_u64;
    source.validate_child_binding(generations, source_name)?;
    for name in
        platform::directory_entries(&source.file, &source.path, MAX_REPUBLISH_DIRECTORY_ENTRIES)?
    {
        let name_text = name
            .to_str()
            .ok_or(IndexError::CurrentRepublishSourceTopology(
                "non-UTF-8 directory entry",
            ))?;
        let relative = PathBuf::from(&name);
        validate_single_component(&relative)?;
        let opened = open_bound_file(source, &relative)?;
        if active.contains(&relative) {
            seen_active.insert(relative.clone());
            admit_clone_resource(
                &mut total_files,
                &mut total_bytes,
                opened.identity.bytes,
                MAX_REPUBLISH_CLONE_FILES,
                MAX_REPUBLISH_CLONE_BYTES,
            )?;
            planned.insert(
                relative.clone(),
                PlannedFile {
                    path: relative,
                    identity: opened.identity,
                    permissions: opened.permissions,
                },
            );
        } else if name_text == MANAGED_FILE {
            if opened.identity.bytes > MAX_MANAGED_METADATA_BYTES {
                return Err(IndexError::CurrentRepublishByteLimit {
                    actual: opened.identity.bytes,
                    maximum: MAX_MANAGED_METADATA_BYTES,
                });
            }
            managed_seen = true;
            admit_clone_resource(
                &mut total_files,
                &mut total_bytes,
                opened.identity.bytes,
                MAX_REPUBLISH_CLONE_FILES,
                MAX_REPUBLISH_CLONE_BYTES,
            )?;
            planned.insert(
                relative.clone(),
                PlannedFile {
                    path: relative,
                    identity: opened.identity,
                    permissions: opened.permissions,
                },
            );
        } else if TANTIVY_LOCK_FILES.contains(&name_text) && opened.identity.bytes == 0 {
            continue;
        } else {
            return Err(IndexError::CurrentRepublishSourceTopology(
                "unexpected directory entry",
            ));
        }
    }
    source.validate_child_binding(generations, source_name)?;
    if seen_active != active || !managed_seen {
        return Err(IndexError::CurrentRepublishSourceTopology(
            "active or managed file missing",
        ));
    }

    let managed =
        planned
            .get(Path::new(MANAGED_FILE))
            .ok_or(IndexError::CurrentRepublishSourceTopology(
                "managed file missing",
            ))?;
    let managed_bytes = read_planned_file(source, managed, MAX_MANAGED_METADATA_BYTES)?;
    let managed_paths: Vec<PathBuf> = serde_json::from_slice(&managed_bytes)
        .map_err(|_| IndexError::CurrentRepublishSourceTopology("invalid managed metadata"))?;
    for path in &managed_paths {
        validate_single_component(path)?;
    }
    let managed_set = managed_paths.iter().cloned().collect::<BTreeSet<_>>();
    if managed_set.len() != managed_paths.len() || managed_set != active {
        return Err(IndexError::CurrentRepublishSourceTopology(
            "managed metadata does not match active files",
        ));
    }

    let required_headroom = total_bytes
        .checked_add(REPUBLISH_HEADROOM_RESERVE_BYTES)
        .ok_or(IndexError::CountOverflow)?;
    Ok(ClonePlan {
        files: planned.into_values().collect(),
        logical_bytes: total_bytes,
        required_headroom,
        managed_bytes,
    })
}

struct OpenedFile {
    file: File,
    identity: FileIdentity,
    permissions: Permissions,
}

fn open_bound_file(directory: &BoundDirectory, relative: &Path) -> Result<OpenedFile> {
    validate_single_component(relative)?;
    let file = platform::open_regular_file_at(&directory.file, &directory.path, relative)
        .map_err(source_topology_open_error)?;
    let metadata = file.metadata()?;
    require_regular(entry_kind(&metadata)?)?;
    let identity = FileIdentity::from_file(&file)?;
    let permissions = metadata.permissions();
    validate_named_file(directory, relative, &identity)?;
    Ok(OpenedFile {
        file,
        identity,
        permissions,
    })
}

fn open_planned_file(directory: &BoundDirectory, planned: &PlannedFile) -> Result<File> {
    let opened = open_bound_file(directory, &planned.path)?;
    if opened.identity != planned.identity {
        return Err(IndexError::CurrentRepublishSourceTopology(
            "source file changed after authentication",
        ));
    }
    Ok(opened.file)
}

fn read_planned_file(
    directory: &BoundDirectory,
    planned: &PlannedFile,
    maximum: u64,
) -> Result<Vec<u8>> {
    if planned.identity.bytes > maximum {
        return Err(IndexError::CurrentRepublishByteLimit {
            actual: planned.identity.bytes,
            maximum,
        });
    }
    let mut file = open_planned_file(directory, planned)?;
    let allocation = usize::try_from(planned.identity.bytes).map_err(|_| {
        IndexError::CurrentRepublishByteLimit {
            actual: planned.identity.bytes,
            maximum,
        }
    })?;
    let mut bytes = Vec::with_capacity(allocation);
    Read::by_ref(&mut file)
        .take(planned.identity.bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 != planned.identity.bytes {
        return Err(IndexError::CurrentRepublishSourceTopology(
            "source file size changed while reading",
        ));
    }
    validate_open_and_named_file(directory, planned, &file)?;
    Ok(bytes)
}

fn clone_files(
    generations: &BoundDirectory,
    source_name: &Path,
    source: &BoundDirectory,
    destination_name: &Path,
    destination: &BoundDirectory,
    plan: &ClonePlan,
) -> Result<()> {
    let mut copied_bytes = 0_u64;
    for planned in &plan.files {
        source.validate_child_binding(generations, source_name)?;
        clone_checkpoint(PortableCloneStage::BeforeCopy, &planned.path)?;
        if planned.path == Path::new(MANAGED_FILE) {
            destination.validate_child_binding(generations, destination_name)?;
            let copied = write_authenticated_plan_bytes(destination, planned, &plan.managed_bytes)?;
            copied_bytes = copied_bytes
                .checked_add(copied)
                .ok_or(IndexError::CountOverflow)?;
            clone_checkpoint(PortableCloneStage::AfterCopy, &planned.path)?;
            continue;
        }
        let mut source_file = open_planned_file(source, planned)?;
        clone_checkpoint(PortableCloneStage::AfterSourceOpen, &planned.path)?;
        destination.validate_child_binding(generations, destination_name)?;
        let mut destination_file =
            platform::create_regular_file_at(&destination.file, &destination.path, &planned.path)?;

        let remaining_allowance = plan.logical_bytes.checked_sub(copied_bytes).ok_or(
            IndexError::CurrentRepublishByteLimit {
                actual: copied_bytes,
                maximum: plan.logical_bytes,
            },
        )?;
        let (copied, source_digest) = copy_with_digest(
            &mut source_file,
            &mut destination_file,
            planned.identity.bytes,
            remaining_allowance,
        )?;
        destination_file.flush()?;
        destination_file.set_permissions(candidate_permissions(&planned.permissions))?;
        destination_file.sync_all()?;
        if copied != planned.identity.bytes {
            return Err(IndexError::CurrentRepublishSourceTopology(
                "copy byte count does not match authenticated source",
            ));
        }
        copied_bytes = copied_bytes
            .checked_add(copied)
            .ok_or(IndexError::CountOverflow)?;
        if copied_bytes > MAX_REPUBLISH_CLONE_BYTES || copied_bytes > plan.logical_bytes {
            return Err(IndexError::CurrentRepublishByteLimit {
                actual: copied_bytes,
                maximum: plan.logical_bytes.min(MAX_REPUBLISH_CLONE_BYTES),
            });
        }

        validate_open_and_named_file(source, planned, &source_file)?;
        let destination_opened = open_bound_file(destination, &planned.path)?;
        if destination_opened.identity.bytes != planned.identity.bytes
            || destination_opened.identity.permissions
                != candidate_permission_identity(planned.identity.permissions)
        {
            return Err(IndexError::CurrentRepublishSourceTopology(
                "copied file metadata does not match authenticated source",
            ));
        }
        let destination_digest = digest_exact_file(
            destination,
            &planned.path,
            &destination_opened.identity,
            destination_opened.file,
        )?;
        if destination_digest != source_digest {
            return Err(IndexError::ChecksumMismatch);
        }
        clone_checkpoint(PortableCloneStage::AfterCopy, &planned.path)?;
        drop(destination_file);
    }
    record_clone_metrics(copied_bytes, plan.files.len());
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn clone_candidate_files(
    root: &Path,
    source_path: &Path,
    predecessor_pointer: &ActiveGenerationPointer,
    certified: &CertifiedPhysicalIntegrity,
    generations: &BoundDirectory,
    source_name: &Path,
    source: &BoundDirectory,
    destination_name: &Path,
    destination: &BoundDirectory,
    plan: &ClonePlan,
    writer_output_headroom: u64,
    physical_proof: &mut CandidatePhysicalProof,
    metrics: &mut CandidateCloneMetrics,
) -> Result<()> {
    let mut copied_bytes = 0_u64;
    for planned in &plan.files {
        source.validate_child_binding(generations, source_name)?;
        let remaining_copy_bytes = plan
            .logical_bytes
            .checked_sub(copied_bytes)
            .ok_or(IndexError::CountOverflow)?;
        let required = remaining_copy_bytes
            .checked_add(writer_output_headroom)
            .ok_or(IndexError::CountOverflow)?;
        admit_available_bytes(generations, required, true)?;
        clone_checkpoint(PortableCloneStage::BeforeCopy, &planned.path)?;
        if planned.path == Path::new(MANAGED_FILE) {
            destination.validate_child_binding(generations, destination_name)?;
            let copied = write_authenticated_plan_bytes(destination, planned, &plan.managed_bytes)?;
            copied_bytes = copied_bytes
                .checked_add(copied)
                .ok_or(IndexError::CountOverflow)?;
            if copied_bytes > MAX_REPUBLISH_CLONE_BYTES || copied_bytes > plan.logical_bytes {
                return Err(IndexError::CurrentRepublishByteLimit {
                    actual: copied_bytes,
                    maximum: plan.logical_bytes.min(MAX_REPUBLISH_CLONE_BYTES),
                });
            }
            clone_checkpoint(PortableCloneStage::AfterCopy, &planned.path)?;
            continue;
        }

        let (expected_artifact, expected_sha256, _sealed) = certified
            .certified_artifact(&planned.path)
            .ok_or(IndexError::ChecksumMismatch)?;
        let (mut source_file, source_before) = open_authenticated_artifact(
            root,
            source_path,
            &planned.path,
            Some(predecessor_pointer),
        )?;
        if source_before != expected_artifact {
            return if expected_artifact.same_payload_identity_changed(&source_before) {
                Err(IndexError::ConcurrentGenerationChange)
            } else {
                Err(IndexError::ChecksumMismatch)
            };
        }
        clone_checkpoint(PortableCloneStage::AfterSourceOpen, &planned.path)?;
        destination.validate_child_binding(generations, destination_name)?;
        let mut destination_file =
            platform::create_regular_file_at(&destination.file, &destination.path, &planned.path)?;
        let remaining_allowance = plan.logical_bytes.checked_sub(copied_bytes).ok_or(
            IndexError::CurrentRepublishByteLimit {
                actual: copied_bytes,
                maximum: plan.logical_bytes,
            },
        )?;
        let (copied, source_digest) = copy_with_digest(
            &mut source_file,
            &mut destination_file,
            source_before.identity.length(),
            remaining_allowance,
        )?;
        if source_digest != expected_sha256 {
            return Err(IndexError::ChecksumMismatch);
        }
        destination_file.flush()?;
        destination_file.set_permissions(candidate_permissions(&planned.permissions))?;
        destination_file.sync_all()?;
        if copied != source_before.identity.length() {
            return Err(IndexError::CurrentRepublishSourceTopology(
                "copy byte count does not match authenticated source",
            ));
        }
        copied_bytes = copied_bytes
            .checked_add(copied)
            .ok_or(IndexError::CountOverflow)?;
        if copied_bytes > MAX_REPUBLISH_CLONE_BYTES || copied_bytes > plan.logical_bytes {
            return Err(IndexError::CurrentRepublishByteLimit {
                actual: copied_bytes,
                maximum: plan.logical_bytes.min(MAX_REPUBLISH_CLONE_BYTES),
            });
        }

        let source_after = recapture_authenticated_artifact(
            root,
            source_path,
            &planned.path,
            &source_file,
            Some(predecessor_pointer),
        )?;
        if source_after != expected_artifact {
            return if expected_artifact.same_payload_identity_changed(&source_after) {
                Err(IndexError::ConcurrentGenerationChange)
            } else {
                Err(IndexError::ChecksumMismatch)
            };
        }
        let destination_opened = open_bound_file(destination, &planned.path)?;
        if destination_opened.identity.bytes != planned.identity.bytes
            || destination_opened.identity.permissions
                != candidate_permission_identity(planned.identity.permissions)
        {
            return Err(IndexError::CurrentRepublishSourceTopology(
                "copied file metadata does not match authenticated source",
            ));
        }
        let destination_artifact =
            capture_artifact_identity(root, &destination.path, &planned.path, None)?;
        physical_proof.insert(PhysicalFileDigest {
            artifact: destination_artifact,
            sha256: expected_sha256,
        });
        metrics.retained_copied_files = metrics
            .retained_copied_files
            .checked_add(1)
            .ok_or(IndexError::CountOverflow)?;
        metrics.retained_copied_bytes = metrics
            .retained_copied_bytes
            .checked_add(copied)
            .ok_or(IndexError::CountOverflow)?;
        clone_checkpoint(PortableCloneStage::AfterCopy, &planned.path)?;
        drop(destination_file);
    }
    admit_available_bytes(generations, writer_output_headroom, true)?;
    Ok(())
}

fn write_authenticated_plan_bytes(
    destination: &BoundDirectory,
    planned: &PlannedFile,
    bytes: &[u8],
) -> Result<u64> {
    let mut destination_file =
        platform::create_regular_file_at(&destination.file, &destination.path, &planned.path)?;
    destination_file.write_all(bytes)?;
    destination_file.flush()?;
    destination_file.set_permissions(candidate_permissions(&planned.permissions))?;
    destination_file.sync_all()?;
    let copied = u64::try_from(bytes.len()).map_err(|_| IndexError::CountOverflow)?;
    let destination_opened = open_bound_file(destination, &planned.path)?;
    if destination_opened.identity.bytes != copied
        || destination_opened.identity.permissions
            != candidate_permission_identity(planned.identity.permissions)
    {
        return Err(IndexError::CurrentRepublishSourceTopology(
            "plan byte count does not match copied control file",
        ));
    }
    Ok(copied)
}

#[cfg(windows)]
fn candidate_permissions(source: &Permissions) -> Permissions {
    let mut candidate = source.clone();
    candidate.set_readonly(false);
    candidate
}

#[cfg(not(windows))]
fn candidate_permissions(source: &Permissions) -> Permissions {
    source.clone()
}

#[cfg(windows)]
fn candidate_permission_identity(_source: PermissionIdentity) -> PermissionIdentity {
    false
}

#[cfg(not(windows))]
fn candidate_permission_identity(source: PermissionIdentity) -> PermissionIdentity {
    source
}

fn copy_with_digest<R: Read, W: Write>(
    source: &mut R,
    destination: &mut W,
    expected_bytes: u64,
    aggregate_allowance: u64,
) -> Result<(u64, [u8; 32])> {
    if expected_bytes > aggregate_allowance {
        return Err(IndexError::CurrentRepublishByteLimit {
            actual: expected_bytes,
            maximum: aggregate_allowance,
        });
    }
    let mut digest = Sha256::new();
    let mut copied = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    while copied < expected_bytes {
        let remaining = expected_bytes - copied;
        let read_limit = usize::try_from(remaining.min(buffer.len() as u64))
            .map_err(|_| IndexError::CountOverflow)?;
        let read = source.read(&mut buffer[..read_limit])?;
        if read == 0 {
            return Err(IndexError::CurrentRepublishSourceTopology(
                "source file truncated while cloning",
            ));
        }
        digest.update(&buffer[..read]);
        destination.write_all(&buffer[..read])?;
        copied = copied
            .checked_add(read as u64)
            .ok_or(IndexError::CountOverflow)?;
    }
    let mut growth_probe = [0_u8; 1];
    if source.read(&mut growth_probe)? != 0 {
        return Err(IndexError::CurrentRepublishSourceTopology(
            "source file grew while cloning",
        ));
    }
    Ok((copied, digest.finalize().into()))
}

fn digest_exact_file(
    directory: &BoundDirectory,
    relative: &Path,
    expected: &FileIdentity,
    mut file: File,
) -> Result<[u8; 32]> {
    let mut digest = Sha256::new();
    let mut read_bytes = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    while read_bytes < expected.bytes {
        let remaining = expected.bytes - read_bytes;
        let read_limit = usize::try_from(remaining.min(buffer.len() as u64))
            .map_err(|_| IndexError::CountOverflow)?;
        let read = file.read(&mut buffer[..read_limit])?;
        if read == 0 {
            return Err(IndexError::CurrentRepublishSourceTopology(
                "copied file truncated during verification",
            ));
        }
        digest.update(&buffer[..read]);
        read_bytes = read_bytes
            .checked_add(read as u64)
            .ok_or(IndexError::CountOverflow)?;
    }
    let mut growth_probe = [0_u8; 1];
    if file.read(&mut growth_probe)? != 0 {
        return Err(IndexError::CurrentRepublishSourceTopology(
            "copied file grew during verification",
        ));
    }
    let actual = FileIdentity::from_file(&file)?;
    if &actual != expected {
        return Err(IndexError::CurrentRepublishSourceTopology(
            "copied file changed during verification",
        ));
    }
    validate_named_file(directory, relative, expected)?;
    Ok(digest.finalize().into())
}

fn validate_open_and_named_file(
    directory: &BoundDirectory,
    planned: &PlannedFile,
    file: &File,
) -> Result<()> {
    if FileIdentity::from_file(file)? != planned.identity {
        return Err(IndexError::CurrentRepublishSourceTopology(
            "source file changed while cloning",
        ));
    }
    validate_named_file(directory, &planned.path, &planned.identity)
}

fn validate_named_file(
    directory: &BoundDirectory,
    relative: &Path,
    expected: &FileIdentity,
) -> Result<()> {
    let named = platform::open_regular_file_at(&directory.file, &directory.path, relative)
        .map_err(source_topology_open_error)?;
    if FileIdentity::from_file(&named)? != *expected {
        return Err(IndexError::CurrentRepublishSourceTopology(
            "named file changed after authentication",
        ));
    }
    Ok(())
}

fn entry_kind(metadata: &Metadata) -> Result<EntryKind> {
    if metadata.file_type().is_symlink() || platform::is_unsafe_link_or_provider(metadata) {
        Ok(EntryKind::LinkOrReparse)
    } else if metadata.is_file() {
        Ok(EntryKind::Regular)
    } else if metadata.is_dir() {
        Ok(EntryKind::Directory)
    } else {
        Ok(EntryKind::Special)
    }
}

fn require_regular(kind: EntryKind) -> Result<()> {
    match kind {
        EntryKind::Regular => Ok(()),
        EntryKind::LinkOrReparse => Err(IndexError::CurrentRepublishSourceTopology(
            "symlink, reparse point, or remote-provider file in republish source",
        )),
        EntryKind::Directory | EntryKind::Special => Err(
            IndexError::CurrentRepublishSourceTopology("non-regular directory entry"),
        ),
    }
}

fn require_directory(kind: EntryKind) -> Result<()> {
    match kind {
        EntryKind::Directory => Ok(()),
        EntryKind::LinkOrReparse => Err(IndexError::CurrentRepublishSourceTopology(
            "symlinked, reparse-point, or remote-provider republish directory",
        )),
        EntryKind::Regular | EntryKind::Special => Err(IndexError::CurrentRepublishSourceTopology(
            "republish path is not a directory",
        )),
    }
}

mod resource;
use resource::{admit_available_bytes, source_topology_open_error};

mod test_support;

#[cfg(any(test, feature = "test-support"))]
pub use test_support::{
    PortableCloneMetrics, PortableCloneStage, PortableCloneTestGuard, PortableCloneTestOptions,
};
#[cfg(not(any(test, feature = "test-support")))]
use test_support::PortableCloneStage;
use test_support::{
    clone_checkpoint, record_clone_metrics, record_plan_metrics,
    record_plan_metrics_with_required,
};
#[cfg(any(test, feature = "test-support"))]
pub(super) use test_support::forced_for_test;

#[cfg(unix)]
#[path = "portable/unix.rs"]
mod platform;

#[cfg(windows)]
#[path = "portable/windows.rs"]
mod platform;


#[cfg(test)]
mod tests;
