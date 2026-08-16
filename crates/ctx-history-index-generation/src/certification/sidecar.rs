use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

use crate::retention::{
    ensure_generation_read_lease_coordinator, try_generation_directory_reclaim_authority,
};
use crate::{
    sync_directory, ActiveGenerationPointer, GenerationRetentionLease, GenerationSlot, Result,
};

#[cfg(any(test, feature = "test-support"))]
use super::load_current_pointer;
use super::{CERTIFICATION_DIRECTORY, CERTIFICATION_SUFFIX};

pub(super) fn is_generation_directory_name(name: &str) -> bool {
    name.strip_prefix("generation-").is_some_and(|suffix| {
        suffix.len() == 32
            && suffix
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

pub(super) fn certification_file_name(slot: &GenerationSlot) -> String {
    format!("{}{CERTIFICATION_SUFFIX}", slot.directory())
}

pub(crate) fn certification_path(root: &Path, slot: &GenerationSlot) -> PathBuf {
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
        .collect::<HashSet<_>>();
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
