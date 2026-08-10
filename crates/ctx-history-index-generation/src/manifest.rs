use std::{
    fs::{self, File},
    path::Path,
};

use tantivy::directory::Directory as _;
use uuid::Uuid;

use crate::{
    is_generation_id, manifest_path, sha256_hex, sync_directory, DurableMmapDirectory,
    GenerationError, Result, MANIFEST_DIRECTORY,
};

/// Loads the exact immutable bytes named by a generation manifest digest.
pub fn load_manifest_bytes(root: &Path, generation_id: &str) -> Result<Vec<u8>> {
    let path = manifest_path(root, generation_id);
    let bytes = fs::read(&path).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => GenerationError::MissingManifest(generation_id.to_owned()),
        _ => GenerationError::Io(error),
    })?;
    let actual = sha256_hex(&bytes);
    if actual != generation_id {
        return Err(GenerationError::ManifestDigestMismatch {
            expected: generation_id.to_owned(),
            actual,
        });
    }
    Ok(bytes)
}

/// Durably publishes already-canonical manifest bytes under their SHA-256 id.
pub fn write_manifest_bytes(root: &Path, generation_id: &str, bytes: &[u8]) -> Result<()> {
    let actual = sha256_hex(bytes);
    if actual != generation_id {
        return Err(GenerationError::ManifestDigestMismatch {
            expected: generation_id.to_owned(),
            actual,
        });
    }
    let directory = root.join(MANIFEST_DIRECTORY);
    fs::create_dir_all(&directory)?;
    let path = manifest_path(root, generation_id);
    if path.is_file() {
        let existing = fs::read(&path)?;
        if existing == bytes {
            File::open(&path)?.sync_all()?;
            sync_directory(&directory)?;
            return Ok(());
        }
        let quarantine = directory.join(format!(
            ".{generation_id}.corrupt-{}",
            Uuid::now_v7().simple()
        ));
        fs::rename(&path, quarantine)?;
        sync_directory(&directory)?;
    }

    let durable_directory =
        DurableMmapDirectory::open(root).map_err(tantivy::TantivyError::from)?;
    let relative_path = Path::new(MANIFEST_DIRECTORY).join(format!("{generation_id}.json"));
    durable_directory.atomic_write(&relative_path, bytes)?;
    Ok(())
}

pub fn reclaim_unreferenced_manifests(
    root: &Path,
    retained_generation_ids: &[String],
) -> Result<()> {
    let directory = root.join(MANIFEST_DIRECTORY);
    fs::create_dir_all(&directory)?;
    let mut removed = false;
    for entry in fs::read_dir(&directory)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let file_name = entry.file_name();
        let Some(file_name) = file_name.to_str() else {
            continue;
        };
        let immutable_generation = file_name
            .strip_suffix(".json")
            .filter(|generation_id| is_generation_id(generation_id));
        let corrupt_quarantine = file_name
            .strip_prefix('.')
            .and_then(|name| name.split_once(".corrupt-"))
            .is_some_and(|(generation_id, suffix)| {
                is_generation_id(generation_id) && !suffix.is_empty()
            });
        let obsolete_integrity_sidecar = is_legacy_generation_integrity_sidecar(file_name);
        let should_remove = immutable_generation.is_some_and(|generation_id| {
            !retained_generation_ids
                .iter()
                .any(|retained| retained == generation_id)
        }) || corrupt_quarantine
            || obsolete_integrity_sidecar;
        if should_remove {
            fs::remove_file(entry.path())?;
            removed = true;
        }
    }
    if removed {
        sync_directory(&directory)?;
    }
    Ok(())
}

fn is_legacy_generation_integrity_sidecar(file_name: &str) -> bool {
    let Some(generation_uuid) = file_name
        .strip_prefix("generation-")
        .and_then(|name| name.strip_suffix(".integrity.json"))
    else {
        return false;
    };
    generation_uuid.len() == 32
        && generation_uuid
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_roundtrip_preserves_exact_bytes() {
        let root = tempfile::tempdir().unwrap();
        let bytes = br#"{"manifest_version":8,"indexed_documents":0}"#;
        let generation_id = sha256_hex(bytes);

        write_manifest_bytes(root.path(), &generation_id, bytes).unwrap();

        assert_eq!(
            load_manifest_bytes(root.path(), &generation_id).unwrap(),
            bytes
        );
        assert_eq!(
            fs::read(manifest_path(root.path(), &generation_id)).unwrap(),
            bytes
        );
    }

    #[test]
    fn manifest_digest_mismatch_fails_closed() {
        let root = tempfile::tempdir().unwrap();
        let expected = "0".repeat(64);

        assert!(matches!(
            write_manifest_bytes(root.path(), &expected, b"not the expected manifest"),
            Err(GenerationError::ManifestDigestMismatch { expected: value, .. }) if value == expected
        ));
    }

    #[test]
    fn reclamation_retains_only_named_manifest_bytes() {
        let root = tempfile::tempdir().unwrap();
        let retained_bytes = b"retained";
        let reclaimed_bytes = b"reclaimed";
        let retained = sha256_hex(retained_bytes);
        let reclaimed = sha256_hex(reclaimed_bytes);
        write_manifest_bytes(root.path(), &retained, retained_bytes).unwrap();
        write_manifest_bytes(root.path(), &reclaimed, reclaimed_bytes).unwrap();
        fs::write(
            root.path()
                .join(MANIFEST_DIRECTORY)
                .join("generation-0123456789abcdef0123456789abcdef.integrity.json"),
            b"obsolete",
        )
        .unwrap();

        reclaim_unreferenced_manifests(root.path(), std::slice::from_ref(&retained)).unwrap();

        assert!(manifest_path(root.path(), &retained).is_file());
        assert!(!manifest_path(root.path(), &reclaimed).exists());
        assert_eq!(
            load_manifest_bytes(root.path(), &retained).unwrap(),
            retained_bytes
        );
    }
}
