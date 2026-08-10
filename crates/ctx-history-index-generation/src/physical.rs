use std::{
    collections::BTreeSet,
    io::Read,
    path::{Path, PathBuf},
};

use crc32fast::Hasher as Crc32;
use sha2::{Digest, Sha256};
use tantivy::{
    directory::{footer::Footer, Directory as _},
    index::SegmentComponent,
    HasLen,
};

use crate::{
    certification::{open_artifact, recapture_artifact, ArtifactIdentity},
    hex, ActiveGenerationPointer, DurableMmapDirectory, GenerationError as IndexError, Result,
};

#[cfg(any(test, feature = "test-support"))]
use std::cell::Cell;

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static CHECKSUM_WALKS: Cell<usize> = const { Cell::new(0) };
    static HASHED_ARTIFACT_BYTES: Cell<u64> = const { Cell::new(0) };
}

const PHYSICAL_INTEGRITY_DOMAIN: &[u8] = b"ctx-tantivy-physical-integrity-v1\0";
const PHYSICAL_HASH_BUFFER_BYTES: usize = 64 * 1024;
const TANTIVY_META_FILE: &str = "meta.json";

#[derive(Debug)]
struct PhysicalFileDigest {
    artifact: ArtifactIdentity,
    sha256: [u8; 32],
}

#[derive(Debug)]
struct PhysicalDigestPart {
    path: String,
    length: u64,
    sha256: [u8; 32],
}

#[derive(Debug)]
pub struct PhysicalIntegrityAudit {
    digest: String,
    artifacts: Vec<ArtifactIdentity>,
}

impl PhysicalIntegrityAudit {
    pub fn digest(&self) -> &str {
        &self.digest
    }

    pub(super) fn artifacts(&self) -> &[ArtifactIdentity] {
        &self.artifacts
    }

    pub(super) fn artifact_paths(&self) -> Vec<String> {
        self.artifacts
            .iter()
            .map(|artifact| artifact.path.clone())
            .collect()
    }
}

/// Computes one physical generation's canonical integrity digest.
///
/// The domain-separated stream contains the exact active file count followed
/// by each sorted UTF-8 relative path, file length, and SHA-256 of the complete
/// file bytes. The sorted path set always includes `meta.json` and every segment
/// file referenced by its active segment metadata. Managed bookkeeping, locks,
/// and temporary files are deliberately excluded because queries do not read them.
/// Segment bytes are streamed once and checked against their Tantivy CRC footer
/// while their stronger SHA-256 is computed.
///
/// `topology_authority` is the caller's already-decoded publication topology.
/// `None` is reserved for a new root or a source-authoritative cold rebuild whose
/// incompatible pointer must remain opaque until the candidate replaces it.
pub fn physical_integrity_digest(
    index: &tantivy::Index,
    generation_path: &Path,
    topology_authority: Option<&ActiveGenerationPointer>,
) -> Result<String> {
    Ok(physical_integrity_audit(index, generation_path, topology_authority)?.digest)
}

pub fn physical_integrity_audit(
    index: &tantivy::Index,
    generation_path: &Path,
    topology_authority: Option<&ActiveGenerationPointer>,
) -> Result<PhysicalIntegrityAudit> {
    #[cfg(any(test, feature = "test-support"))]
    CHECKSUM_WALKS.with(|count| count.set(count.get() + 1));
    let directory =
        DurableMmapDirectory::open(generation_path).map_err(|_| IndexError::ChecksumMismatch)?;
    let root = generation_path
        .parent()
        .filter(|parent| {
            parent
                .file_name()
                .is_some_and(|name| name == "index-generations")
        })
        .and_then(Path::parent)
        .ok_or(IndexError::ChecksumMismatch)?;
    let mut paths = active_index_files(index)?;
    paths.insert(PathBuf::from(TANTIVY_META_FILE));
    let entries = paths
        .into_iter()
        .map(|path| {
            hash_physical_file(
                root,
                &directory,
                generation_path,
                &path,
                path != Path::new(TANTIVY_META_FILE),
                topology_authority,
            )
        })
        .collect::<Result<Vec<_>>>()?;
    let parts = entries
        .iter()
        .map(|entry| PhysicalDigestPart {
            path: entry.artifact.path.clone(),
            length: entry.artifact.identity.length(),
            sha256: entry.sha256,
        })
        .collect::<Vec<_>>();
    let digest = canonical_physical_integrity_digest(&parts)?;
    let artifacts = entries.into_iter().map(|entry| entry.artifact).collect();
    Ok(PhysicalIntegrityAudit { digest, artifacts })
}

/// Verifies a generation against the physical authority in its pointer slot.
pub fn verify_physical_integrity(
    index: &tantivy::Index,
    generation_path: &Path,
    topology_authority: Option<&ActiveGenerationPointer>,
    expected_digest: &str,
) -> Result<()> {
    let audit = physical_integrity_audit(index, generation_path, topology_authority)?;
    if audit.digest != expected_digest {
        return Err(IndexError::ChecksumMismatch);
    }
    Ok(())
}

fn hash_physical_file(
    root: &Path,
    directory: &DurableMmapDirectory,
    generation_path: &Path,
    relative_path: &Path,
    validate_tantivy_footer: bool,
    pointer: Option<&ActiveGenerationPointer>,
) -> Result<PhysicalFileDigest> {
    let (mut file, artifact) = open_artifact(root, generation_path, relative_path, pointer)?;
    let length = artifact.identity.length();
    let footer_contract = if validate_tantivy_footer {
        let slice = directory
            .open_read(relative_path)
            .map_err(|_| IndexError::ChecksumMismatch)?;
        let (footer, body) =
            Footer::extract_footer(slice).map_err(|_| IndexError::ChecksumMismatch)?;
        Some((
            u64::try_from(body.len()).map_err(|_| IndexError::CountOverflow)?,
            footer.crc,
        ))
    } else {
        None
    };

    let mut sha256 = Sha256::new();
    let mut crc32 = footer_contract.map(|_| Crc32::new());
    let mut body_remaining = footer_contract.map_or(0, |(body_length, _)| body_length);
    let mut bytes_read = 0_u64;
    let mut buffer = [0_u8; PHYSICAL_HASH_BUFFER_BYTES];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|_| IndexError::ChecksumMismatch)?;
        if count == 0 {
            break;
        }
        let count_u64 = u64::try_from(count).map_err(|_| IndexError::CountOverflow)?;
        bytes_read = bytes_read
            .checked_add(count_u64)
            .ok_or(IndexError::CountOverflow)?;
        #[cfg(any(test, feature = "test-support"))]
        HASHED_ARTIFACT_BYTES.with(|bytes| bytes.set(bytes.get().saturating_add(count_u64)));
        sha256.update(&buffer[..count]);
        if let Some(crc32) = crc32.as_mut() {
            let body_count = usize::try_from(body_remaining.min(count_u64))
                .map_err(|_| IndexError::CountOverflow)?;
            crc32.update(&buffer[..body_count]);
            body_remaining -= u64::try_from(body_count).map_err(|_| IndexError::CountOverflow)?;
        }
    }
    if bytes_read != length || body_remaining != 0 {
        return Err(IndexError::ChecksumMismatch);
    }
    if let (Some(crc32), Some((_, expected_crc32))) = (crc32, footer_contract) {
        if crc32.finalize() != expected_crc32 {
            return Err(IndexError::ChecksumMismatch);
        }
    }
    let recaptured = recapture_artifact(root, generation_path, relative_path, pointer)?;
    if recaptured != artifact {
        return if artifact.same_payload_identity_changed(&recaptured) {
            Err(IndexError::ConcurrentGenerationChange)
        } else {
            Err(IndexError::ChecksumMismatch)
        };
    }
    Ok(PhysicalFileDigest {
        artifact,
        sha256: sha256.finalize().into(),
    })
}

fn canonical_physical_integrity_digest(entries: &[PhysicalDigestPart]) -> Result<String> {
    let mut digest = Sha256::new();
    digest.update(PHYSICAL_INTEGRITY_DOMAIN);
    digest.update(
        u64::try_from(entries.len())
            .map_err(|_| IndexError::CountOverflow)?
            .to_be_bytes(),
    );
    for entry in entries {
        let path = entry.path.as_bytes();
        digest.update(
            u64::try_from(path.len())
                .map_err(|_| IndexError::CountOverflow)?
                .to_be_bytes(),
        );
        digest.update(path);
        digest.update(entry.length.to_be_bytes());
        digest.update(entry.sha256);
    }
    Ok(hex(&digest.finalize()))
}

#[cfg(test)]
#[test]
fn sha256_physical_authority_distinguishes_stubbed_crc_collision() {
    struct StubbedCrcFile<'a> {
        path: &'a str,
        bytes: &'a [u8],
        crc32: u32,
    }

    let first = StubbedCrcFile {
        path: "same.store",
        bytes: b"certificate-A",
        crc32: 0xfeed_beef,
    };
    let second = StubbedCrcFile {
        path: "same.store",
        bytes: b"certificate-B",
        crc32: 0xfeed_beef,
    };
    assert_eq!(first.path, second.path);
    assert_eq!(first.bytes.len(), second.bytes.len());
    assert_eq!(first.crc32, second.crc32);

    let entry = |file: &StubbedCrcFile<'_>| PhysicalDigestPart {
        path: file.path.to_owned(),
        length: u64::try_from(file.bytes.len()).unwrap(),
        sha256: Sha256::digest(file.bytes).into(),
    };
    assert_ne!(
        canonical_physical_integrity_digest(&[entry(&first)]).unwrap(),
        canonical_physical_integrity_digest(&[entry(&second)]).unwrap()
    );
}

pub fn active_index_files(index: &tantivy::Index) -> Result<BTreeSet<PathBuf>> {
    let directory = index.directory();
    let metas = index.load_metas()?;
    let mut expected_files = BTreeSet::new();
    for segment in &metas.segments {
        for component in [
            SegmentComponent::Postings,
            SegmentComponent::FastFields,
            SegmentComponent::FieldNorms,
            SegmentComponent::Terms,
            SegmentComponent::Store,
        ] {
            expected_files.insert(segment.relative_path(component));
        }
        let positions = segment.relative_path(SegmentComponent::Positions);
        if directory
            .exists(&positions)
            .map_err(|_| IndexError::ChecksumMismatch)?
        {
            expected_files.insert(positions);
        }
        if segment.has_deletes() {
            expected_files.insert(segment.relative_path(SegmentComponent::Delete));
        }
    }
    Ok(expected_files)
}

#[cfg(any(test, feature = "test-support"))]
pub fn reset_physical_verification_activity() {
    CHECKSUM_WALKS.with(|count| count.set(0));
    HASHED_ARTIFACT_BYTES.with(|bytes| bytes.set(0));
}

#[cfg(any(test, feature = "test-support"))]
pub fn checksum_walks() -> usize {
    CHECKSUM_WALKS.with(Cell::get)
}

#[cfg(any(test, feature = "test-support"))]
pub fn hashed_artifact_bytes() -> u64 {
    HASHED_ARTIFACT_BYTES.with(Cell::get)
}
