use std::path::Path;

use tantivy::Index;

use crate::{ActiveGenerationPointer, CandidateGeneration, GenerationError as IndexError, Result};

#[cfg(not(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "windows",
    target_os = "freebsd"
)))]
compile_error!("predecessor republish clone is only qualified on ctx release targets");

mod candidate;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod exact_copy;
#[cfg(any(
    test,
    feature = "test-support",
    target_os = "windows",
    target_os = "freebsd"
))]
mod portable;

pub use candidate::{CandidateActivationFence, RepublishCandidate};

pub(super) const MAX_REPUBLISH_CLONE_FILES: usize = 4_096;
pub(super) const MAX_REPUBLISH_CLONE_BYTES: u64 = 1024 * 1024 * 1024 * 1024;
const MAX_REPUBLISH_DIRECTORY_ENTRIES: usize = 4_096;
const REPUBLISH_HEADROOM_RESERVE_BYTES: u64 = 16 * 1024 * 1024;
const MANAGED_FILE: &str = ".managed.json";
const TANTIVY_LOCK_FILES: [&str; 2] = [".tantivy-meta.lock", ".tantivy-writer.lock"];

pub fn create_authenticated_republish_candidate(
    root: &Path,
    predecessor_pointer: &ActiveGenerationPointer,
    predecessor_index: &Index,
) -> Result<RepublishCandidate> {
    #[cfg(any(test, feature = "test-support"))]
    if portable::forced_for_test() {
        let candidate = portable::create_authenticated_republish_candidate(
            root,
            predecessor_pointer,
            predecessor_index,
        )?;
        return Ok(RepublishCandidate::new(candidate));
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        let candidate = unix::create_authenticated_republish_candidate(
            root,
            predecessor_pointer,
            predecessor_index,
        )?;
        Ok(RepublishCandidate::new(candidate))
    }
    #[cfg(any(target_os = "windows", target_os = "freebsd"))]
    {
        let candidate = portable::create_authenticated_republish_candidate(
            root,
            predecessor_pointer,
            predecessor_index,
        )?;
        Ok(RepublishCandidate::new(candidate))
    }
}

pub fn create_authenticated_candidate_generation(
    root: &Path,
    predecessor_pointer: &ActiveGenerationPointer,
    predecessor_index: &Index,
    writer_memory_bytes: u64,
) -> Result<CandidateGeneration> {
    #[cfg(any(test, feature = "test-support"))]
    if portable::forced_for_test() {
        return portable::create_authenticated_candidate_generation(
            root,
            predecessor_pointer,
            predecessor_index,
            writer_memory_bytes,
        );
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        unix::create_authenticated_candidate_generation(
            root,
            predecessor_pointer,
            predecessor_index,
            writer_memory_bytes,
        )
    }
    #[cfg(any(target_os = "windows", target_os = "freebsd"))]
    {
        portable::create_authenticated_candidate_generation(
            root,
            predecessor_pointer,
            predecessor_index,
            writer_memory_bytes,
        )
    }
}

pub(crate) fn bind_candidate_activation_fence(
    root: &Path,
    directory_name: &Path,
) -> Result<CandidateActivationFence> {
    #[cfg(any(test, feature = "test-support"))]
    if portable::forced_for_test() {
        return portable::CandidateGuard::bind(root, directory_name)
            .map(CandidateActivationFence::portable);
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        unix::CandidateGuard::bind(root, directory_name)
            .map(CandidateActivationFence::descriptor_clone)
    }
    #[cfg(any(target_os = "windows", target_os = "freebsd"))]
    {
        portable::CandidateGuard::bind(root, directory_name).map(CandidateActivationFence::portable)
    }
}

#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CandidateCloneMetrics {
    pub retained_reflinked_files: usize,
    pub retained_hardlinked_files: usize,
    pub retained_copied_files: usize,
    pub retained_copied_bytes: u64,
}

#[cfg(not(any(test, feature = "test-support")))]
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct CandidateCloneMetrics {
    retained_reflinked_files: usize,
    retained_hardlinked_files: usize,
    retained_copied_files: usize,
    retained_copied_bytes: u64,
}

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static CANDIDATE_CLONE_METRICS: std::cell::Cell<CandidateCloneMetrics> = const {
        std::cell::Cell::new(CandidateCloneMetrics {
            retained_reflinked_files: 0,
            retained_hardlinked_files: 0,
            retained_copied_files: 0,
            retained_copied_bytes: 0,
        })
    };
}

#[cfg(any(test, feature = "test-support"))]
fn record_candidate_clone_metrics(metrics: CandidateCloneMetrics) {
    CANDIDATE_CLONE_METRICS.with(|slot| slot.set(metrics));
}

#[cfg(not(any(test, feature = "test-support")))]
fn record_candidate_clone_metrics(_metrics: CandidateCloneMetrics) {}

#[cfg(any(test, feature = "test-support"))]
pub fn reset_candidate_clone_metrics() {
    CANDIDATE_CLONE_METRICS.with(|slot| slot.set(CandidateCloneMetrics::default()));
}

#[cfg(any(test, feature = "test-support"))]
pub fn candidate_clone_metrics() -> CandidateCloneMetrics {
    CANDIDATE_CLONE_METRICS.with(std::cell::Cell::get)
}

fn validate_single_component(path: &Path) -> Result<()> {
    let mut components = path.components();
    if !matches!(components.next(), Some(std::path::Component::Normal(_)))
        || components.next().is_some()
    {
        return Err(IndexError::CurrentRepublishSourceTopology(
            "managed path escapes generation directory",
        ));
    }
    Ok(())
}

fn admit_clone_resource(
    files: &mut usize,
    bytes: &mut u64,
    next_bytes: u64,
    maximum_files: usize,
    maximum_bytes: u64,
) -> Result<()> {
    *files = files.checked_add(1).ok_or(IndexError::CountOverflow)?;
    if *files > maximum_files {
        return Err(IndexError::CurrentRepublishFileLimit {
            actual: *files,
            maximum: maximum_files,
        });
    }
    *bytes = bytes
        .checked_add(next_bytes)
        .ok_or(IndexError::CountOverflow)?;
    if *bytes > maximum_bytes {
        return Err(IndexError::CurrentRepublishByteLimit {
            actual: *bytes,
            maximum: maximum_bytes,
        });
    }
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod unix {
    mod guard;

    use std::{
        collections::{BTreeMap, BTreeSet},
        ffi::{CStr, CString, OsString},
        fs::{self, File},
        io::{self, Read, Seek, SeekFrom, Write},
        os::{
            fd::{AsRawFd, FromRawFd, RawFd},
            unix::{ffi::OsStringExt, fs::MetadataExt},
        },
        path::{Path, PathBuf},
    };

    use tantivy::Index;
    use uuid::Uuid;

    use super::exact_copy::{
        copy_and_hash_exact_authenticated_file, copy_exact_authenticated_file,
    };
    use super::{
        admit_clone_resource, record_candidate_clone_metrics, validate_single_component,
        CandidateActivationFence, CandidateCloneMetrics, MANAGED_FILE, MAX_REPUBLISH_CLONE_BYTES,
        MAX_REPUBLISH_CLONE_FILES, MAX_REPUBLISH_DIRECTORY_ENTRIES,
        REPUBLISH_HEADROOM_RESERVE_BYTES, TANTIVY_LOCK_FILES,
    };
    use crate::{
        active_index_files,
        certification::{
            capture_artifact_identity, open_authenticated_artifact,
            recapture_authenticated_artifact,
        },
        lexical_index_settings,
        physical::{PhysicalFileDigest, MAX_MANAGED_METADATA_BYTES},
        physical_integrity_digest, verify_or_certify_physical_integrity, ActiveGenerationPointer,
        CandidateGeneration, CandidatePhysicalProof, CertifiedPhysicalIntegrity,
        DurableMmapDirectory, GenerationError as IndexError, Result, INDEX_GENERATIONS_DIRECTORY,
    };
    pub(super) use guard::CandidateGuard;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct FileIdentity {
        device: u64,
        inode: u64,
        bytes: u64,
        mode: u64,
    }

    #[cfg(target_os = "linux")]
    const fn normalized_stat_device(device: libc::dev_t) -> u64 {
        device
    }

    #[cfg(target_os = "macos")]
    const fn normalized_stat_device(device: libc::dev_t) -> u64 {
        device as u64
    }

    impl FileIdentity {
        fn from_metadata(metadata: &fs::Metadata) -> Self {
            Self {
                device: metadata.dev(),
                inode: metadata.ino(),
                bytes: metadata.len(),
                mode: u64::from(metadata.mode()),
            }
        }

        fn from_stat(stat: &libc::stat) -> Self {
            Self {
                device: normalized_stat_device(stat.st_dev),
                inode: stat.st_ino,
                bytes: u64::try_from(stat.st_size).unwrap_or(u64::MAX),
                mode: u64::from(stat.st_mode),
            }
        }

        fn is_regular(self) -> bool {
            self.mode & u64::from(libc::S_IFMT) == u64::from(libc::S_IFREG)
        }

        fn is_directory(self) -> bool {
            self.mode & u64::from(libc::S_IFMT) == u64::from(libc::S_IFDIR)
        }

        fn is_same_object(self, other: Self) -> bool {
            self.device == other.device
                && self.inode == other.inode
                && (self.mode & u64::from(libc::S_IFMT)) == (other.mode & u64::from(libc::S_IFMT))
        }
    }

    #[cfg(all(test, target_os = "macos"))]
    #[test]
    fn signed_darwin_device_id_normalization_preserves_distinct_values() {
        assert_ne!(normalized_stat_device(-1), normalized_stat_device(-2));
    }

    #[derive(Debug, Clone)]
    struct PlannedFile {
        path: PathBuf,
        identity: FileIdentity,
        copy_required: bool,
    }

    struct ClonePlan {
        files: Vec<PlannedFile>,
        logical_bytes: u64,
        required_headroom: u64,
        control_copy_bytes: u64,
        managed_bytes: Vec<u8>,
    }

    impl ClonePlan {
        fn writer_output_headroom(&self, writer_memory_bytes: u64) -> Result<u64> {
            self.logical_bytes
                .checked_add(writer_memory_bytes)
                .and_then(|bytes| bytes.checked_add(REPUBLISH_HEADROOM_RESERVE_BYTES))
                .ok_or(IndexError::CountOverflow)
        }

        fn initial_candidate_headroom(&self, writer_memory_bytes: u64) -> Result<u64> {
            self.control_copy_bytes
                .checked_add(self.writer_output_headroom(writer_memory_bytes)?)
                .ok_or(IndexError::CountOverflow)
        }

        fn full_copy_candidate_headroom(&self, writer_memory_bytes: u64) -> Result<u64> {
            self.logical_bytes
                .checked_add(self.writer_output_headroom(writer_memory_bytes)?)
                .ok_or(IndexError::CountOverflow)
        }
    }

    struct BoundDirectory {
        file: File,
        identity: FileIdentity,
    }

    impl BoundDirectory {
        fn open_path(path: &Path) -> Result<Self> {
            let file = open_path_nofollow(path, libc::O_RDONLY | libc::O_DIRECTORY)
                .map_err(source_topology_open_error)?;
            Self::from_file(file)
        }

        fn open_at(parent: &File, name: &Path) -> Result<Self> {
            let file =
                open_at_nofollow(parent.as_raw_fd(), name, libc::O_RDONLY | libc::O_DIRECTORY)
                    .map_err(source_topology_open_error)?;
            Self::from_file(file)
        }

        fn from_file(file: File) -> Result<Self> {
            let identity = FileIdentity::from_metadata(&file.metadata()?);
            if !identity.is_directory() {
                return Err(IndexError::CurrentRepublishSourceTopology(
                    "generation path is not a directory",
                ));
            }
            Ok(Self { file, identity })
        }
    }

    pub(super) fn create_authenticated_republish_candidate(
        root: &Path,
        predecessor_pointer: &ActiveGenerationPointer,
        predecessor_index: &Index,
    ) -> Result<CandidateGeneration> {
        let base = predecessor_pointer.active();
        let root_path = root.to_path_buf();
        let root_directory = BoundDirectory::open_path(root)?;
        validate_path_binding(root, root_directory.identity)?;
        let generations_name = PathBuf::from(INDEX_GENERATIONS_DIRECTORY);
        let generations_path = root.join(INDEX_GENERATIONS_DIRECTORY);
        let generations = BoundDirectory::open_at(&root_directory.file, &generations_name)?;
        validate_child_binding(
            &root_directory.file,
            &generations_name,
            generations.identity,
        )?;
        validate_path_binding(&generations_path, generations.identity)?;
        let source_name = Path::new(base.directory());
        validate_single_component(source_name)?;
        let source = BoundDirectory::open_at(&generations.file, source_name)?;
        validate_child_binding(&generations.file, source_name, source.identity)?;

        let plan = authenticated_clone_plan(&source, predecessor_index)?;
        let available = available_bytes(&generations.file, false)?;
        record_plan_metrics(&plan, available);
        if available < plan.required_headroom {
            return Err(IndexError::CurrentRepublishInsufficientHeadroom {
                available,
                required: plan.required_headroom,
            });
        }

        let directory_name = format!("generation-{}", Uuid::now_v7().simple());
        let destination_name = PathBuf::from(&directory_name);
        create_directory_at(&generations.file, &destination_name)?;
        let destination_path = generations_path.join(&directory_name);
        let destination = BoundDirectory::open_at(&generations.file, &destination_name)?;
        validate_child_binding(&generations.file, &destination_name, destination.identity)?;
        let guard = CandidateGuard {
            root_path,
            root: root_directory,
            generations_name,
            generations_path,
            generations,
            destination_name,
            destination,
        };
        let clone_result = (|| {
            clone_files(
                &guard.generations,
                source_name,
                &source,
                &guard.destination,
                &plan,
            )?;
            guard.generations.file.sync_all()?;
            validate_child_binding(&guard.generations.file, source_name, source.identity)?;
            guard.validate_binding()?;

            let directory = DurableMmapDirectory::open(&destination_path)
                .map_err(tantivy::TantivyError::from)?;
            let index = Index::open(directory)?;
            if index.settings() != &lexical_index_settings() {
                return Err(IndexError::IndexSettingsMismatch);
            }
            let cloned_digest =
                physical_integrity_digest(&index, &destination_path, Some(predecessor_pointer))?;
            if cloned_digest != base.physical_integrity_digest() {
                return Err(IndexError::ChecksumMismatch);
            }
            guard.validate_binding()?;
            Ok((
                directory_name.clone(),
                index,
                CandidatePhysicalProof::default(),
            ))
        })();
        match clone_result {
            Ok((directory_name, index, physical_proof)) => Ok(CandidateGeneration {
                directory_name,
                index,
                physical_proof,
                activation_fence: CandidateActivationFence::descriptor_clone(guard),
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
        let certified = verify_or_certify_physical_integrity(
            root,
            predecessor_pointer,
            base,
            predecessor_index,
        )?;
        let root_path = root.to_path_buf();
        let root_directory = BoundDirectory::open_path(root)?;
        validate_path_binding(root, root_directory.identity)?;
        let generations_name = PathBuf::from(INDEX_GENERATIONS_DIRECTORY);
        let generations_path = root.join(INDEX_GENERATIONS_DIRECTORY);
        let generations = BoundDirectory::open_at(&root_directory.file, &generations_name)?;
        validate_child_binding(
            &root_directory.file,
            &generations_name,
            generations.identity,
        )?;
        validate_path_binding(&generations_path, generations.identity)?;
        let source_name = Path::new(base.directory());
        validate_single_component(source_name)?;
        let source_path = generations_path.join(source_name);
        let source = BoundDirectory::open_at(&generations.file, source_name)?;
        validate_child_binding(&generations.file, source_name, source.identity)?;

        let plan = authenticated_clone_plan(&source, predecessor_index)?;
        let required_headroom = plan.initial_candidate_headroom(writer_memory_bytes)?;
        let full_copy_headroom = plan.full_copy_candidate_headroom(writer_memory_bytes)?;
        let writer_output_headroom = plan.writer_output_headroom(writer_memory_bytes)?;
        let available = available_bytes(&generations.file, false)?;
        record_plan_metrics_with_required(&plan, available, full_copy_headroom);
        if available < required_headroom {
            return Err(IndexError::CurrentRepublishInsufficientHeadroom {
                available,
                required: required_headroom,
            });
        }

        let directory_name = format!("generation-{}", Uuid::now_v7().simple());
        let destination_name = PathBuf::from(&directory_name);
        create_directory_at(&generations.file, &destination_name)?;
        let destination_path = generations_path.join(&directory_name);
        let destination = BoundDirectory::open_at(&generations.file, &destination_name)?;
        validate_child_binding(&generations.file, &destination_name, destination.identity)?;
        let guard = CandidateGuard {
            root_path,
            root: root_directory,
            generations_name,
            generations_path,
            generations,
            destination_name,
            destination,
        };
        let clone_result = (|| {
            let mut physical_proof = CandidatePhysicalProof::default();
            let mut metrics = CandidateCloneMetrics::default();
            clone_candidate_files(
                root,
                &source_path,
                &destination_path,
                predecessor_pointer,
                &certified,
                &guard.generations,
                source_name,
                &source,
                &guard.destination,
                &plan,
                writer_output_headroom,
                &mut physical_proof,
                &mut metrics,
            )?;
            guard.generations.file.sync_all()?;
            validate_child_binding(&guard.generations.file, source_name, source.identity)?;
            guard.validate_binding()?;

            let directory = DurableMmapDirectory::open(&destination_path)
                .map_err(tantivy::TantivyError::from)?;
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
                activation_fence: CandidateActivationFence::descriptor_clone(guard),
            }),
            Err(error) => {
                guard.discard();
                Err(error)
            }
        }
    }

    fn authenticated_clone_plan(source: &BoundDirectory, index: &Index) -> Result<ClonePlan> {
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
        for name in directory_entries(&source.file, MAX_REPUBLISH_DIRECTORY_ENTRIES)? {
            let name_text = name
                .to_str()
                .ok_or(IndexError::CurrentRepublishSourceTopology(
                    "non-UTF-8 directory entry",
                ))?;
            let relative = PathBuf::from(&name);
            validate_single_component(&relative)?;
            let file = open_regular_file_at(&source.file, &relative)?;
            let identity = FileIdentity::from_metadata(&file.metadata()?);
            validate_file_binding(&source.file, &relative, identity)?;
            if active.contains(&relative) {
                seen_active.insert(relative.clone());
                admit_clone_resource(
                    &mut total_files,
                    &mut total_bytes,
                    identity.bytes,
                    MAX_REPUBLISH_CLONE_FILES,
                    MAX_REPUBLISH_CLONE_BYTES,
                )?;
                planned.insert(
                    relative.clone(),
                    PlannedFile {
                        copy_required: relative == Path::new("meta.json"),
                        path: relative,
                        identity,
                    },
                );
            } else if name_text == MANAGED_FILE {
                if identity.bytes > MAX_MANAGED_METADATA_BYTES {
                    return Err(IndexError::CurrentRepublishByteLimit {
                        actual: identity.bytes,
                        maximum: MAX_MANAGED_METADATA_BYTES,
                    });
                }
                managed_seen = true;
                admit_clone_resource(
                    &mut total_files,
                    &mut total_bytes,
                    identity.bytes,
                    MAX_REPUBLISH_CLONE_FILES,
                    MAX_REPUBLISH_CLONE_BYTES,
                )?;
                planned.insert(
                    relative.clone(),
                    PlannedFile {
                        path: relative,
                        identity,
                        copy_required: true,
                    },
                );
            } else if TANTIVY_LOCK_FILES.contains(&name_text) && identity.bytes == 0 {
                continue;
            } else {
                return Err(IndexError::CurrentRepublishSourceTopology(
                    "unexpected directory entry",
                ));
            }
        }
        if seen_active != active || !managed_seen {
            return Err(IndexError::CurrentRepublishSourceTopology(
                "active or managed file missing",
            ));
        }

        let managed = planned.get(Path::new(MANAGED_FILE)).ok_or(
            IndexError::CurrentRepublishSourceTopology("managed file missing"),
        )?;
        let managed_bytes = read_bound_file(source, managed, MAX_MANAGED_METADATA_BYTES)?;
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
        let control_copy_bytes = planned
            .values()
            .filter(|file| file.copy_required)
            .try_fold(0_u64, |bytes, file| {
                bytes
                    .checked_add(file.identity.bytes)
                    .ok_or(IndexError::CountOverflow)
            })?;
        Ok(ClonePlan {
            files: planned.into_values().collect(),
            logical_bytes: total_bytes,
            required_headroom,
            control_copy_bytes,
            managed_bytes,
        })
    }

    fn read_bound_file(
        directory: &BoundDirectory,
        planned: &PlannedFile,
        maximum: u64,
    ) -> Result<Vec<u8>> {
        let mut file = open_regular_file_at(&directory.file, &planned.path)?;
        let before = FileIdentity::from_metadata(&file.metadata()?);
        if before != planned.identity {
            return Err(IndexError::CurrentRepublishSourceTopology(
                "source file changed after authentication",
            ));
        }
        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take(maximum.saturating_add(1))
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 != planned.identity.bytes {
            return Err(IndexError::CurrentRepublishSourceTopology(
                "source file size changed while reading",
            ));
        }
        validate_file_binding(&directory.file, &planned.path, planned.identity)?;
        Ok(bytes)
    }

    fn clone_files(
        generations: &BoundDirectory,
        source_name: &Path,
        source: &BoundDirectory,
        destination: &BoundDirectory,
        plan: &ClonePlan,
    ) -> Result<()> {
        let mut actual_copied_bytes = 0_u64;
        let mut linked_files = 0_usize;
        let mut copied_files = 0_usize;
        for planned in &plan.files {
            validate_child_binding(&generations.file, source_name, source.identity)?;
            clone_checkpoint(CloneStage::BeforeFile, &planned.path)?;
            validate_child_binding(&generations.file, source_name, source.identity)?;
            if planned.path == Path::new(MANAGED_FILE) {
                clone_checkpoint(CloneStage::BeforeCopy, &planned.path)?;
                let copied = write_authenticated_plan_bytes(
                    &destination.file,
                    &planned.path,
                    &plan.managed_bytes,
                )?;
                actual_copied_bytes = actual_copied_bytes
                    .checked_add(copied)
                    .ok_or(IndexError::CountOverflow)?;
                copied_files = copied_files
                    .checked_add(1)
                    .ok_or(IndexError::CountOverflow)?;
                validate_child_binding(&generations.file, source_name, source.identity)?;
                clone_checkpoint(CloneStage::AfterFile, &planned.path)?;
                continue;
            }
            let mut source_file = open_regular_file_at(&source.file, &planned.path)?;
            let before = FileIdentity::from_metadata(&source_file.metadata()?);
            if before != planned.identity {
                return Err(IndexError::CurrentRepublishSourceTopology(
                    "source file changed after authentication",
                ));
            }
            validate_file_binding(&source.file, &planned.path, before)?;

            let force_copy = planned.copy_required || force_copy_fallback();
            if !force_copy {
                clone_checkpoint(CloneStage::BeforeHardlink, &planned.path)?;
            }
            let linked = !force_copy
                && match hard_link_at(&source.file, &planned.path, &destination.file) {
                    Ok(()) => true,
                    Err(error) if hardlink_copy_fallback_error(&error) => false,
                    Err(error) => return Err(error.into()),
                };
            if linked {
                let linked_file = open_regular_file_at(&destination.file, &planned.path)?;
                let linked_identity = FileIdentity::from_metadata(&linked_file.metadata()?);
                if linked_identity != before {
                    return Err(IndexError::CurrentRepublishSourceTopology(
                        "hardlink target identity does not match authenticated source",
                    ));
                }
                linked_files = linked_files
                    .checked_add(1)
                    .ok_or(IndexError::CountOverflow)?;
            } else {
                clone_checkpoint(CloneStage::BeforeCopy, &planned.path)?;
                source_file.seek(SeekFrom::Start(0))?;
                let remaining_allowance = plan
                    .logical_bytes
                    .checked_sub(actual_copied_bytes)
                    .ok_or(IndexError::CurrentRepublishByteLimit {
                        actual: actual_copied_bytes,
                        maximum: plan.logical_bytes,
                    })?;
                let mut destination_file =
                    create_regular_file_at(&destination.file, &planned.path)?;
                let copied = copy_exact_authenticated_file(
                    &mut source_file,
                    &mut destination_file,
                    before.bytes,
                    remaining_allowance,
                )?;
                destination_file.flush()?;
                let destination_identity =
                    FileIdentity::from_metadata(&destination_file.metadata()?);
                if copied != before.bytes || destination_identity.bytes != before.bytes {
                    return Err(IndexError::CurrentRepublishSourceTopology(
                        "copy byte count does not match authenticated source",
                    ));
                }
                actual_copied_bytes = actual_copied_bytes
                    .checked_add(copied)
                    .ok_or(IndexError::CountOverflow)?;
                if actual_copied_bytes > MAX_REPUBLISH_CLONE_BYTES
                    || actual_copied_bytes > plan.logical_bytes
                {
                    return Err(IndexError::CurrentRepublishByteLimit {
                        actual: actual_copied_bytes,
                        maximum: plan.logical_bytes.min(MAX_REPUBLISH_CLONE_BYTES),
                    });
                }
                copied_files = copied_files
                    .checked_add(1)
                    .ok_or(IndexError::CountOverflow)?;
            }
            let after = FileIdentity::from_metadata(&source_file.metadata()?);
            if after != before {
                return Err(IndexError::CurrentRepublishSourceTopology(
                    "source file changed while cloning",
                ));
            }
            validate_file_binding(&source.file, &planned.path, after)?;
            validate_child_binding(&generations.file, source_name, source.identity)?;
            clone_checkpoint(CloneStage::AfterFile, &planned.path)?;
        }
        record_clone_metrics(actual_copied_bytes, linked_files, copied_files);
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn clone_candidate_files(
        root: &Path,
        source_path: &Path,
        destination_path: &Path,
        predecessor_pointer: &ActiveGenerationPointer,
        certified: &CertifiedPhysicalIntegrity,
        generations: &BoundDirectory,
        source_name: &Path,
        source: &BoundDirectory,
        destination: &BoundDirectory,
        plan: &ClonePlan,
        writer_output_headroom: u64,
        physical_proof: &mut CandidatePhysicalProof,
        metrics: &mut CandidateCloneMetrics,
    ) -> Result<()> {
        let mut actual_copied_bytes = 0_u64;
        for planned in &plan.files {
            validate_child_binding(&generations.file, source_name, source.identity)?;
            clone_checkpoint(CloneStage::BeforeFile, &planned.path)?;
            validate_child_binding(&generations.file, source_name, source.identity)?;
            if planned.path == Path::new(MANAGED_FILE) {
                clone_checkpoint(CloneStage::BeforeCopy, &planned.path)?;
                let copied = write_authenticated_plan_bytes(
                    &destination.file,
                    &planned.path,
                    &plan.managed_bytes,
                )?;
                actual_copied_bytes = actual_copied_bytes
                    .checked_add(copied)
                    .ok_or(IndexError::CountOverflow)?;
                if actual_copied_bytes > MAX_REPUBLISH_CLONE_BYTES
                    || actual_copied_bytes > plan.logical_bytes
                {
                    return Err(IndexError::CurrentRepublishByteLimit {
                        actual: actual_copied_bytes,
                        maximum: plan.logical_bytes.min(MAX_REPUBLISH_CLONE_BYTES),
                    });
                }
                validate_child_binding(&generations.file, source_name, source.identity)?;
                clone_checkpoint(CloneStage::AfterFile, &planned.path)?;
                continue;
            }

            let (expected_artifact, expected_sha256, sealed) = certified
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
            clone_checkpoint(CloneStage::AfterSourceOpen, &planned.path)?;
            if sealed
                && !planned.copy_required
                && !force_reflink_fallback()
                && try_clone_reflink_at(&source_file, &destination.file, &planned.path)?
            {
                metrics.retained_reflinked_files = metrics
                    .retained_reflinked_files
                    .checked_add(1)
                    .ok_or(IndexError::CountOverflow)?;
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
                let destination_artifact =
                    capture_artifact_identity(root, destination_path, &planned.path, None)?;
                physical_proof.insert(PhysicalFileDigest {
                    artifact: destination_artifact,
                    sha256: expected_sha256,
                });
                validate_child_binding(&generations.file, source_name, source.identity)?;
                clone_checkpoint(CloneStage::AfterFile, &planned.path)?;
                continue;
            }

            clone_checkpoint(CloneStage::BeforeHardlink, &planned.path)?;
            let source_prelink = recapture_authenticated_artifact(
                root,
                source_path,
                &planned.path,
                &source_file,
                Some(predecessor_pointer),
            )?;
            if source_prelink != expected_artifact {
                return if expected_artifact.same_payload_identity_changed(&source_prelink) {
                    Err(IndexError::ConcurrentGenerationChange)
                } else {
                    Err(IndexError::ChecksumMismatch)
                };
            }
            let before = FileIdentity::from_metadata(&source_file.metadata()?);
            let linked = sealed
                && !planned.copy_required
                && !force_hardlink_fallback()
                && !force_copy_fallback()
                && match hard_link_authenticated_source(
                    &source.file,
                    &planned.path,
                    &destination.file,
                ) {
                    Ok(()) => true,
                    Err(error) if hardlink_copy_fallback_error(&error) => false,
                    Err(error) => return Err(error.into()),
                };
            if linked {
                let linked_file = open_regular_file_at(&destination.file, &planned.path)?;
                let linked_identity = FileIdentity::from_metadata(&linked_file.metadata()?);
                if linked_identity != before {
                    return Err(IndexError::CurrentRepublishSourceTopology(
                        "hardlink target identity does not match authenticated source",
                    ));
                }
                validate_file_binding(&source.file, &planned.path, before)?;
                let destination_artifact =
                    capture_artifact_identity(root, destination_path, &planned.path, None)?;
                physical_proof.insert(PhysicalFileDigest {
                    artifact: destination_artifact,
                    sha256: expected_sha256,
                });
                metrics.retained_hardlinked_files = metrics
                    .retained_hardlinked_files
                    .checked_add(1)
                    .ok_or(IndexError::CountOverflow)?;
                validate_child_binding(&generations.file, source_name, source.identity)?;
                clone_checkpoint(CloneStage::AfterFile, &planned.path)?;
                continue;
            }

            let admitted_copy_bytes = if planned.copy_required {
                source_before.identity.length()
            } else {
                plan.logical_bytes
                    .checked_sub(actual_copied_bytes)
                    .ok_or(IndexError::CountOverflow)?
            };
            let required = admitted_copy_bytes
                .checked_add(writer_output_headroom)
                .ok_or(IndexError::CountOverflow)?;
            admit_available_bytes(&generations.file, required, true)?;
            clone_checkpoint(CloneStage::BeforeCopy, &planned.path)?;
            source_file.seek(SeekFrom::Start(0))?;
            let remaining_allowance = plan.logical_bytes.checked_sub(actual_copied_bytes).ok_or(
                IndexError::CurrentRepublishByteLimit {
                    actual: actual_copied_bytes,
                    maximum: plan.logical_bytes,
                },
            )?;
            let mut destination_file = create_regular_file_at(&destination.file, &planned.path)?;
            let (copied, copied_sha256) = copy_and_hash_exact_authenticated_file(
                &mut source_file,
                &mut destination_file,
                source_before.identity.length(),
                remaining_allowance,
            )?;
            if copied_sha256 != expected_sha256 {
                return Err(IndexError::ChecksumMismatch);
            }
            destination_file.flush()?;
            let destination_identity = FileIdentity::from_metadata(&destination_file.metadata()?);
            if copied != source_before.identity.length()
                || destination_identity.bytes != source_before.identity.length()
            {
                return Err(IndexError::CurrentRepublishSourceTopology(
                    "copy byte count does not match authenticated source",
                ));
            }
            actual_copied_bytes = actual_copied_bytes
                .checked_add(copied)
                .ok_or(IndexError::CountOverflow)?;
            if actual_copied_bytes > MAX_REPUBLISH_CLONE_BYTES
                || actual_copied_bytes > plan.logical_bytes
            {
                return Err(IndexError::CurrentRepublishByteLimit {
                    actual: actual_copied_bytes,
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
            let destination_artifact =
                capture_artifact_identity(root, destination_path, &planned.path, None)?;
            physical_proof.insert(PhysicalFileDigest {
                artifact: destination_artifact,
                sha256: expected_sha256,
            });
            if !planned.copy_required {
                metrics.retained_copied_files = metrics
                    .retained_copied_files
                    .checked_add(1)
                    .ok_or(IndexError::CountOverflow)?;
                metrics.retained_copied_bytes = metrics
                    .retained_copied_bytes
                    .checked_add(copied)
                    .ok_or(IndexError::CountOverflow)?;
            }
            validate_child_binding(&generations.file, source_name, source.identity)?;
            clone_checkpoint(CloneStage::AfterFile, &planned.path)?;
        }
        admit_available_bytes(&generations.file, writer_output_headroom, true)?;
        Ok(())
    }

    fn write_authenticated_plan_bytes(
        destination: &File,
        path: &Path,
        bytes: &[u8],
    ) -> Result<u64> {
        let mut destination_file = create_regular_file_at(destination, path)?;
        destination_file.write_all(bytes)?;
        destination_file.flush()?;
        let copied = u64::try_from(bytes.len()).map_err(|_| IndexError::CountOverflow)?;
        if FileIdentity::from_metadata(&destination_file.metadata()?).bytes != copied {
            return Err(IndexError::CurrentRepublishSourceTopology(
                "plan byte count does not match copied control file",
            ));
        }
        Ok(copied)
    }

    fn discard_bound_directory(
        generations: &BoundDirectory,
        destination_name: &Path,
        destination: &BoundDirectory,
    ) -> Result<()> {
        for name in directory_entries(&destination.file, MAX_REPUBLISH_DIRECTORY_ENTRIES)? {
            let relative = Path::new(&name);
            validate_single_component(relative)?;
            let file = open_regular_file_at(&destination.file, relative)?;
            let identity = FileIdentity::from_metadata(&file.metadata()?);
            validate_file_binding(&destination.file, relative, identity)?;
            unlink_at(&destination.file, relative, 0)?;
        }
        validate_child_binding(&generations.file, destination_name, destination.identity)?;
        unlink_at(&generations.file, destination_name, libc::AT_REMOVEDIR)
    }

    fn unlink_at(parent: &File, path: &Path, flags: libc::c_int) -> Result<()> {
        let path = path_cstring(path)?;
        // SAFETY: the parent descriptor and NUL-terminated relative path stay
        // live for the call. Callers retain and revalidate the opened target.
        if unsafe { libc::unlinkat(parent.as_raw_fd(), path.as_ptr(), flags) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error().into())
        }
    }

    fn open_regular_file_at(directory: &File, path: &Path) -> Result<File> {
        let file = open_at_nofollow(directory.as_raw_fd(), path, libc::O_RDONLY)
            .map_err(source_topology_open_error)?;
        let identity = FileIdentity::from_metadata(&file.metadata()?);
        if !identity.is_regular() {
            return Err(IndexError::CurrentRepublishSourceTopology(
                "non-regular directory entry",
            ));
        }
        Ok(file)
    }

    fn create_regular_file_at(directory: &File, path: &Path) -> io::Result<File> {
        let path = path_cstring(path)?;
        // SAFETY: `path` is NUL-terminated, the directory descriptor remains
        // open, and successful ownership is transferred into `File` exactly once.
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                path.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                0o600,
            )
        };
        file_from_fd(fd)
    }

    fn open_path_nofollow(path: &Path, flags: libc::c_int) -> io::Result<File> {
        let path = path_cstring(path)?;
        // SAFETY: `path` is NUL-terminated and successful descriptor ownership
        // is transferred into `File` exactly once.
        let fd = unsafe { libc::open(path.as_ptr(), flags | libc::O_CLOEXEC | libc::O_NOFOLLOW) };
        file_from_fd(fd)
    }

    fn open_at_nofollow(directory: RawFd, path: &Path, flags: libc::c_int) -> io::Result<File> {
        let path = path_cstring(path)?;
        // SAFETY: `path` is NUL-terminated, `directory` is borrowed for the
        // call, and successful descriptor ownership transfers exactly once.
        let fd = unsafe {
            libc::openat(
                directory,
                path.as_ptr(),
                flags | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        file_from_fd(fd)
    }

    fn file_from_fd(fd: libc::c_int) -> io::Result<File> {
        if fd < 0 {
            Err(io::Error::last_os_error())
        } else {
            // SAFETY: a nonnegative `open`/`openat` result is a newly owned fd.
            Ok(unsafe { File::from_raw_fd(fd) })
        }
    }

    fn path_cstring(path: &Path) -> io::Result<CString> {
        use std::os::unix::ffi::OsStrExt;
        CString::new(path.as_os_str().as_bytes()).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "path contains an interior NUL")
        })
    }

    fn create_directory_at(parent: &File, path: &Path) -> Result<()> {
        let path = path_cstring(path)?;
        // SAFETY: `path` is NUL-terminated and `parent` remains open.
        if unsafe { libc::mkdirat(parent.as_raw_fd(), path.as_ptr(), 0o700) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error().into())
        }
    }

    fn hard_link_at(source: &File, path: &Path, destination: &File) -> io::Result<()> {
        let path = path_cstring(path)?;
        // SAFETY: both descriptors and both NUL-terminated path pointers stay
        // valid for the duration of `linkat`.
        if unsafe {
            libc::linkat(
                source.as_raw_fd(),
                path.as_ptr(),
                destination.as_raw_fd(),
                path.as_ptr(),
                0,
            )
        } == 0
        {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    #[cfg(target_os = "linux")]
    fn hard_link_authenticated_source(
        source_directory: &File,
        path: &Path,
        destination: &File,
    ) -> io::Result<()> {
        hard_link_at(source_directory, path, destination)
    }

    #[cfg(target_os = "macos")]
    fn hard_link_authenticated_source(
        _source: &File,
        _path: &Path,
        _destination: &File,
    ) -> io::Result<()> {
        Err(io::Error::from_raw_os_error(libc::EOPNOTSUPP))
    }

    #[cfg(target_os = "linux")]
    fn try_clone_reflink_at(source: &File, destination: &File, path: &Path) -> Result<bool> {
        let destination_file = create_regular_file_at(destination, path)?;
        let result = unsafe {
            libc::ioctl(
                destination_file.as_raw_fd(),
                libc::FICLONE,
                source.as_raw_fd(),
            )
        };
        if result == 0 {
            return Ok(true);
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error().is_some_and(|code| {
            [
                libc::EOPNOTSUPP,
                libc::ENOTTY,
                libc::EINVAL,
                libc::EXDEV,
                libc::EPERM,
                libc::EACCES,
            ]
            .contains(&code)
        }) {
            drop(destination_file);
            let _ = unlink_at(destination, path, 0);
            return Ok(false);
        }
        Err(error.into())
    }

    #[cfg(target_os = "macos")]
    fn try_clone_reflink_at(source: &File, destination: &File, path: &Path) -> Result<bool> {
        let path = path_cstring(path)?;
        let result = unsafe {
            libc::fclonefileat(
                source.as_raw_fd(),
                destination.as_raw_fd(),
                path.as_ptr(),
                0,
            )
        };
        if result == 0 {
            return Ok(true);
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error().is_some_and(|code| {
            [
                libc::ENOTSUP,
                libc::EINVAL,
                libc::EXDEV,
                libc::EPERM,
                libc::EACCES,
            ]
            .contains(&code)
        }) {
            return Ok(false);
        }
        Err(error.into())
    }

    fn hardlink_copy_fallback_error(error: &io::Error) -> bool {
        error.raw_os_error().is_some_and(|code| {
            [
                libc::EXDEV,
                libc::EPERM,
                libc::EACCES,
                libc::EMLINK,
                libc::EOPNOTSUPP,
                libc::ENOENT,
            ]
            .contains(&code)
        })
    }

    fn validate_path_binding(path: &Path, expected: FileIdentity) -> Result<()> {
        let metadata = fs::symlink_metadata(path).map_err(source_topology_open_error)?;
        let actual = FileIdentity::from_metadata(&metadata);
        if !actual.is_directory() || !actual.is_same_object(expected) {
            return Err(IndexError::CurrentRepublishSourceTopology(
                "generation parent path changed during republish",
            ));
        }
        Ok(())
    }

    fn validate_child_binding(parent: &File, path: &Path, expected: FileIdentity) -> Result<()> {
        let actual = stat_at(parent, path)?;
        if !actual.is_directory() || !actual.is_same_object(expected) {
            return Err(IndexError::CurrentRepublishSourceTopology(
                "active generation directory changed during republish",
            ));
        }
        Ok(())
    }

    fn validate_file_binding(parent: &File, path: &Path, expected: FileIdentity) -> Result<()> {
        let actual = stat_at(parent, path)?;
        if !actual.is_regular() || actual != expected {
            return Err(IndexError::CurrentRepublishSourceTopology(
                "source file changed during republish",
            ));
        }
        Ok(())
    }

    fn stat_at(parent: &File, path: &Path) -> Result<FileIdentity> {
        let path = path_cstring(path)?;
        // SAFETY: zeroed `stat` is initialized by a successful `fstatat`; the
        // descriptor and path remain valid for the call.
        let mut stat = unsafe { std::mem::zeroed::<libc::stat>() };
        let result = unsafe {
            libc::fstatat(
                parent.as_raw_fd(),
                path.as_ptr(),
                &mut stat,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if result == 0 {
            Ok(FileIdentity::from_stat(&stat))
        } else {
            Err(source_topology_open_error(io::Error::last_os_error()))
        }
    }

    fn directory_entries(directory: &File, maximum: usize) -> Result<Vec<OsString>> {
        // SAFETY: `dup` creates an independently owned descriptor.
        let duplicate = unsafe { libc::dup(directory.as_raw_fd()) };
        if duplicate < 0 {
            return Err(io::Error::last_os_error().into());
        }
        // SAFETY: `fdopendir` consumes `duplicate` on success.
        let stream = unsafe { libc::fdopendir(duplicate) };
        if stream.is_null() {
            // SAFETY: `fdopendir` did not consume the descriptor on failure.
            unsafe { libc::close(duplicate) };
            return Err(io::Error::last_os_error().into());
        }
        struct Stream(*mut libc::DIR);
        impl Drop for Stream {
            fn drop(&mut self) {
                // SAFETY: the stream is uniquely owned and closed once.
                unsafe { libc::closedir(self.0) };
            }
        }
        let stream = Stream(stream);
        let mut entries = Vec::new();
        loop {
            set_errno(0);
            // SAFETY: `stream` remains open and `readdir`'s pointer is consumed
            // before the next call.
            let entry = unsafe { libc::readdir(stream.0) };
            if entry.is_null() {
                let error = io::Error::last_os_error();
                if error.raw_os_error().unwrap_or(0) != 0 {
                    return Err(error.into());
                }
                break;
            }
            // SAFETY: POSIX guarantees NUL termination of `d_name`.
            let bytes = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
            if bytes == b"." || bytes == b".." {
                continue;
            }
            let actual = entries
                .len()
                .checked_add(1)
                .ok_or(IndexError::CountOverflow)?;
            if actual > maximum {
                return Err(IndexError::CurrentRepublishFileLimit { actual, maximum });
            }
            entries.push(OsString::from_vec(bytes.to_vec()));
        }
        entries.sort();
        Ok(entries)
    }

    #[cfg(target_os = "linux")]
    fn set_errno(value: libc::c_int) {
        // SAFETY: the returned pointer addresses this thread's errno.
        unsafe { *libc::__errno_location() = value };
    }

    #[cfg(target_os = "macos")]
    fn set_errno(value: libc::c_int) {
        // SAFETY: the returned pointer addresses this thread's errno.
        unsafe { *libc::__error() = value };
    }

    fn admit_available_bytes(directory: &File, required: u64, recheck: bool) -> Result<()> {
        let available = available_bytes(directory, recheck)?;
        if available < required {
            return Err(IndexError::CurrentRepublishInsufficientHeadroom {
                available,
                required,
            });
        }
        Ok(())
    }

    fn available_bytes(directory: &File, recheck: bool) -> Result<u64> {
        #[cfg(any(test, feature = "test-support"))]
        if let Some(available) = TEST_CLONE_OPTIONS.with(|options| {
            let options = options.borrow();
            if recheck {
                options
                    .rechecked_available_bytes
                    .or(options.available_bytes)
            } else {
                options.available_bytes
            }
        }) {
            return Ok(available);
        }
        #[cfg(not(any(test, feature = "test-support")))]
        let _ = recheck;
        // SAFETY: zeroed `statvfs` is initialized by successful `fstatvfs`.
        let mut stat = unsafe { std::mem::zeroed::<libc::statvfs>() };
        if unsafe { libc::fstatvfs(directory.as_raw_fd(), &mut stat) } != 0 {
            return Err(io::Error::last_os_error().into());
        }
        Ok((stat.f_bavail as u64).saturating_mul(stat.f_frsize as u64))
    }

    fn source_topology_open_error(error: io::Error) -> IndexError {
        if error
            .raw_os_error()
            .is_some_and(|code| [libc::ELOOP, libc::ENOTDIR].contains(&code))
        {
            IndexError::CurrentRepublishSourceTopology(
                "symlinked or non-directory republish source",
            )
        } else {
            IndexError::Io(error)
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum CloneStage {
        BeforeFile,
        AfterSourceOpen,
        BeforeHardlink,
        BeforeCopy,
        AfterFile,
        BeforeCleanup,
    }

    #[cfg(not(any(test, feature = "test-support")))]
    #[derive(Debug, Clone, Copy)]
    enum CloneStage {
        BeforeFile,
        AfterSourceOpen,
        BeforeHardlink,
        BeforeCopy,
        AfterFile,
        BeforeCleanup,
    }

    #[cfg(any(test, feature = "test-support"))]
    #[derive(Debug, Clone, Copy, Default)]
    pub struct CloneTestOptions {
        pub force_copy: bool,
        pub force_reflink_fallback: bool,
        pub force_hardlink_fallback: bool,
        pub available_bytes: Option<u64>,
        pub rechecked_available_bytes: Option<u64>,
    }

    #[cfg(any(test, feature = "test-support"))]
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub struct CloneMetrics {
        pub planned_files: usize,
        pub logical_bytes: u64,
        pub required_headroom: u64,
        pub available_bytes: u64,
        pub copied_bytes: u64,
        pub linked_files: usize,
        pub copied_files: usize,
    }

    #[cfg(any(test, feature = "test-support"))]
    type CloneTestHook = Box<dyn for<'a> FnMut(CloneStage, &'a Path) -> Result<()>>;

    #[cfg(any(test, feature = "test-support"))]
    thread_local! {
        static TEST_CLONE_OPTIONS: std::cell::RefCell<CloneTestOptions> = const {
            std::cell::RefCell::new(CloneTestOptions {
                force_copy: false,
                force_reflink_fallback: false,
                force_hardlink_fallback: false,
                available_bytes: None,
                rechecked_available_bytes: None,
            })
        };
        static TEST_CLONE_HOOK: std::cell::RefCell<Option<CloneTestHook>> =
            std::cell::RefCell::new(None);
        static TEST_CLONE_METRICS: std::cell::Cell<CloneMetrics> = const {
            std::cell::Cell::new(CloneMetrics {
                planned_files: 0,
                logical_bytes: 0,
                required_headroom: 0,
                available_bytes: 0,
                copied_bytes: 0,
                linked_files: 0,
                copied_files: 0,
            })
        };
    }

    #[cfg(any(test, feature = "test-support"))]
    pub struct CloneTestHookGuard {
        previous_options: CloneTestOptions,
        previous_hook: Option<CloneTestHook>,
        previous_metrics: CloneMetrics,
    }

    #[cfg(any(test, feature = "test-support"))]
    impl CloneTestHookGuard {
        pub fn set<F>(options: CloneTestOptions, hook: F) -> Self
        where
            F: for<'a> FnMut(CloneStage, &'a Path) -> Result<()> + 'static,
        {
            let previous_options = TEST_CLONE_OPTIONS.with(|slot| slot.replace(options));
            let previous_hook = TEST_CLONE_HOOK.with(|slot| slot.replace(Some(Box::new(hook))));
            let previous_metrics =
                TEST_CLONE_METRICS.with(|slot| slot.replace(CloneMetrics::default()));
            Self {
                previous_options,
                previous_hook,
                previous_metrics,
            }
        }

        pub fn metrics(&self) -> CloneMetrics {
            TEST_CLONE_METRICS.with(std::cell::Cell::get)
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    impl Drop for CloneTestHookGuard {
        fn drop(&mut self) {
            TEST_CLONE_OPTIONS.with(|slot| slot.replace(self.previous_options));
            TEST_CLONE_HOOK.with(|slot| slot.replace(self.previous_hook.take()));
            TEST_CLONE_METRICS.with(|slot| slot.set(self.previous_metrics));
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    fn force_copy_fallback() -> bool {
        TEST_CLONE_OPTIONS.with(|options| options.borrow().force_copy)
    }

    #[cfg(not(any(test, feature = "test-support")))]
    fn force_copy_fallback() -> bool {
        false
    }

    #[cfg(any(test, feature = "test-support"))]
    fn force_reflink_fallback() -> bool {
        TEST_CLONE_OPTIONS.with(|options| options.borrow().force_reflink_fallback)
    }

    #[cfg(not(any(test, feature = "test-support")))]
    fn force_reflink_fallback() -> bool {
        false
    }

    #[cfg(any(test, feature = "test-support"))]
    fn force_hardlink_fallback() -> bool {
        TEST_CLONE_OPTIONS.with(|options| options.borrow().force_hardlink_fallback)
    }

    #[cfg(not(any(test, feature = "test-support")))]
    fn force_hardlink_fallback() -> bool {
        false
    }

    #[cfg(any(test, feature = "test-support"))]
    fn clone_checkpoint(stage: CloneStage, path: &Path) -> Result<()> {
        TEST_CLONE_HOOK.with(|hook| match hook.borrow_mut().as_mut() {
            Some(hook) => hook(stage, path),
            None => Ok(()),
        })
    }

    #[cfg(not(any(test, feature = "test-support")))]
    fn clone_checkpoint(_stage: CloneStage, _path: &Path) -> Result<()> {
        Ok(())
    }

    #[cfg(any(test, feature = "test-support"))]
    fn record_plan_metrics(plan: &ClonePlan, available: u64) {
        record_plan_metrics_with_required(plan, available, plan.required_headroom);
    }

    #[cfg(any(test, feature = "test-support"))]
    fn record_plan_metrics_with_required(plan: &ClonePlan, available: u64, required_headroom: u64) {
        TEST_CLONE_METRICS.with(|metrics| {
            metrics.set(CloneMetrics {
                planned_files: plan.files.len(),
                logical_bytes: plan.logical_bytes,
                required_headroom,
                available_bytes: available,
                ..metrics.get()
            });
        });
    }

    #[cfg(not(any(test, feature = "test-support")))]
    fn record_plan_metrics(_plan: &ClonePlan, _available: u64) {}

    #[cfg(not(any(test, feature = "test-support")))]
    fn record_plan_metrics_with_required(
        _plan: &ClonePlan,
        _available: u64,
        _required_headroom: u64,
    ) {
    }

    #[cfg(any(test, feature = "test-support"))]
    fn record_clone_metrics(copied_bytes: u64, linked_files: usize, copied_files: usize) {
        TEST_CLONE_METRICS.with(|metrics| {
            metrics.set(CloneMetrics {
                copied_bytes,
                linked_files,
                copied_files,
                ..metrics.get()
            });
        });
    }

    #[cfg(not(any(test, feature = "test-support")))]
    fn record_clone_metrics(_copied_bytes: u64, _linked_files: usize, _copied_files: usize) {}
}

#[cfg(all(
    any(test, feature = "test-support"),
    any(target_os = "linux", target_os = "macos")
))]
pub use unix::{CloneMetrics, CloneStage, CloneTestHookGuard, CloneTestOptions};

#[cfg(any(test, feature = "test-support"))]
pub use portable::{
    PortableCloneMetrics, PortableCloneStage, PortableCloneTestGuard, PortableCloneTestOptions,
};

#[cfg(test)]
mod tests;
