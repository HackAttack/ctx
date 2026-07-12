use std::{
    fs,
    io::{Read, Seek},
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Context, Result};
use sha2::{Digest, Sha256};
use uuid::Uuid;

#[cfg(test)]
use super::semantic_model_acquisition_integrity_error;
use super::{
    cache_paths::semantic_model_cache_roots, SemanticCpuModelCacheMissing,
    SemanticCpuModelIntegrityError, SemanticModelFile, SEMANTIC_HF_MODEL_CACHE_DIR,
    SEMANTIC_MANAGED_MODEL_CACHE_DIR, SEMANTIC_MODEL_ID, SEMANTIC_MODEL_REVISION,
    SEMANTIC_REQUIRED_MODEL_FILES,
};

#[cfg(ctx_semantic_fastembed)]
pub(super) fn semantic_cpu_cache_snapshot(cache_dir: &Path) -> Result<PathBuf> {
    let mut repairable_error = None;
    for model_root in semantic_model_cache_roots(cache_dir) {
        let snapshot = model_root.join("snapshots").join(SEMANTIC_MODEL_REVISION);
        match fs::metadata(&snapshot) {
            Ok(metadata) if metadata.is_dir() => match verify_semantic_cpu_snapshot(&snapshot) {
                Ok(()) => return Ok(snapshot),
                Err(error) if semantic_cpu_cache_repairable(&error) => {
                    repairable_error.get_or_insert(error);
                }
                Err(error) => return Err(error),
            },
            Ok(_) => {
                repairable_error.get_or_insert_with(|| {
                    SemanticCpuModelIntegrityError(format!(
                        "semantic CPU model snapshot {} is not a directory",
                        snapshot.display()
                    ))
                    .into()
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("inspect semantic model cache {}", snapshot.display())
                });
            }
        }
    }
    Err(repairable_error.unwrap_or_else(|| {
        SemanticCpuModelCacheMissing(format!(
            "semantic model cache is incomplete at {}",
            cache_dir.display()
        ))
        .into()
    }))
}

#[cfg(ctx_semantic_fastembed)]
pub(super) fn semantic_cpu_cache_repairable(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<SemanticCpuModelCacheMissing>()
        .is_some()
        || error
            .downcast_ref::<SemanticCpuModelIntegrityError>()
            .is_some()
}

#[cfg(ctx_semantic_fastembed)]
fn verify_semantic_cpu_snapshot(snapshot: &Path) -> Result<()> {
    for expected in SEMANTIC_REQUIRED_MODEL_FILES {
        verify_semantic_cpu_file(&snapshot.join(expected.path), *expected)?;
    }
    Ok(())
}

#[cfg(ctx_semantic_fastembed)]
fn verify_semantic_cpu_file(path: &Path, expected: SemanticModelFile) -> Result<()> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(SemanticCpuModelCacheMissing(format!(
                "semantic CPU model file {} is missing",
                path.display()
            ))
            .into());
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("inspect semantic CPU model file {}", path.display()));
        }
    };
    if !metadata.is_file() || metadata.len() != expected.size {
        return Err(SemanticCpuModelIntegrityError(format!(
            "semantic CPU model file {} has size {}, expected {}",
            path.display(),
            metadata.len(),
            expected.size
        ))
        .into());
    }
    let mut file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(SemanticCpuModelCacheMissing(format!(
                "semantic CPU model file {} disappeared during verification",
                path.display()
            ))
            .into());
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("open semantic CPU model file {}", path.display()));
        }
    };
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 128 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .with_context(|| format!("read semantic CPU model file {}", path.display()))?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    let actual = format!("{:x}", hasher.finalize());
    if actual != expected.sha256 {
        return Err(SemanticCpuModelIntegrityError(format!(
            "semantic CPU model file {} has SHA-256 {actual}, expected {}",
            path.display(),
            expected.sha256
        ))
        .into());
    }
    Ok(())
}

#[cfg(ctx_semantic_fastembed)]
pub(super) fn replace_cpu_model_cache_from_pinned_revision(cache_dir: &Path) -> Result<PathBuf> {
    use hf_hub::{api::sync::ApiBuilder, Repo, RepoType};

    let managed_root = cache_dir.join(SEMANTIC_MANAGED_MODEL_CACHE_DIR);
    fs::create_dir_all(&managed_root)
        .with_context(|| format!("create semantic model cache {}", managed_root.display()))?;
    let _lock = lock_semantic_model_acquisition(&managed_root)?;

    match semantic_cpu_cache_snapshot(cache_dir) {
        Ok(snapshot) => {
            let _ = cleanup_semantic_cpu_download_cache(&managed_root.join("download-cache"));
            return Ok(snapshot);
        }
        Err(error) if semantic_cpu_cache_repairable(&error) => {}
        Err(error) => return Err(error),
    }

    let download_cache = managed_root.join("download-cache");
    let model_root = managed_root.join(SEMANTIC_HF_MODEL_CACHE_DIR);
    let mut verified_staging_root = None;
    for attempt in 0..2 {
        if attempt > 0 {
            cleanup_semantic_cpu_download_cache(&download_cache)?;
        }
        prepare_semantic_cpu_download_cache(&download_cache)?;
        let api = ApiBuilder::new()
            .with_cache_dir(download_cache.clone())
            .with_progress(false)
            .build()
            .context("initialize pinned semantic model downloader")?;
        let repo = api.repo(Repo::with_revision(
            SEMANTIC_MODEL_ID.to_owned(),
            RepoType::Model,
            SEMANTIC_MODEL_REVISION.to_owned(),
        ));
        let staging_root = managed_root.join(format!(
            ".{SEMANTIC_HF_MODEL_CACHE_DIR}.staging-{}",
            Uuid::new_v4().simple()
        ));
        let staging_snapshot = staging_root.join("snapshots").join(SEMANTIC_MODEL_REVISION);
        let staged = (|| -> Result<()> {
            for expected in SEMANTIC_REQUIRED_MODEL_FILES {
                let downloaded = repo.download(expected.path).with_context(|| {
                    format!(
                        "download {SEMANTIC_MODEL_ID}@{SEMANTIC_MODEL_REVISION}/{}",
                        expected.path
                    )
                })?;
                let destination = staging_snapshot.join(expected.path);
                if let Some(parent) = destination.parent() {
                    fs::create_dir_all(parent).with_context(|| {
                        format!(
                            "create semantic model staging directory {}",
                            parent.display()
                        )
                    })?;
                }
                stage_semantic_cpu_model_file(&downloaded, &download_cache, &destination)?;
            }
            verify_semantic_cpu_snapshot(&staging_snapshot).with_context(|| {
                format!(
                    "downloaded semantic CPU model failed verification in {}",
                    staging_snapshot.display()
                )
            })
        })();
        match staged {
            Ok(()) => {
                verified_staging_root = Some(staging_root);
                break;
            }
            Err(error) if attempt == 0 && semantic_cpu_cache_repairable(&error) => {
                let _ = fs::remove_dir_all(&staging_root);
            }
            Err(error) => {
                let _ = fs::remove_dir_all(&staging_root);
                return Err(error);
            }
        }
    }
    let staging_root = verified_staging_root.ok_or_else(|| {
        anyhow!("semantic CPU model download did not produce a verified snapshot")
    })?;

    if let Err(error) = publish_semantic_cpu_model_root(&staging_root, &model_root, &_lock) {
        let _ = fs::remove_dir_all(&staging_root);
        return Err(error);
    }
    Ok(model_root.join("snapshots").join(SEMANTIC_MODEL_REVISION))
}

#[cfg(ctx_semantic_fastembed)]
fn lock_semantic_model_acquisition(managed_root: &Path) -> Result<fs::File> {
    use fs2::FileExt;

    fs::create_dir_all(managed_root)
        .with_context(|| format!("create semantic model cache {}", managed_root.display()))?;
    let lock_path = managed_root.join("acquisition.lock");
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .with_context(|| {
            format!(
                "open semantic model acquisition lock {}",
                lock_path.display()
            )
        })?;
    lock.lock_exclusive()
        .with_context(|| format!("lock semantic model acquisition {}", lock_path.display()))?;
    Ok(lock)
}

#[cfg(ctx_semantic_fastembed)]
pub(super) fn maybe_cleanup_semantic_cpu_download_cache_after_cached_acquisition(
    cache_dir: &Path,
    daemon_owned: bool,
) {
    if !daemon_owned {
        return;
    }
    let managed_root = cache_dir.join(SEMANTIC_MANAGED_MODEL_CACHE_DIR);
    let download_cache = managed_root.join("download-cache");
    if fs::symlink_metadata(&download_cache).is_err() {
        return;
    }
    let Ok(_lock) = lock_semantic_model_acquisition(&managed_root) else {
        return;
    };
    let _ = cleanup_semantic_cpu_download_cache(&download_cache);
}

#[cfg(ctx_semantic_fastembed)]
fn prepare_semantic_cpu_download_cache(download_cache: &Path) -> Result<()> {
    match fs::symlink_metadata(download_cache) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(()),
        Ok(_) => Err(anyhow!(
            "semantic model download cache {} has an unexpected filesystem shape",
            download_cache.display()
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(download_cache).with_context(|| {
                format!(
                    "create semantic model download cache {}",
                    download_cache.display()
                )
            })?;
            let metadata = fs::symlink_metadata(download_cache).with_context(|| {
                format!(
                    "inspect created semantic model download cache {}",
                    download_cache.display()
                )
            })?;
            if metadata.is_dir() && !metadata.file_type().is_symlink() {
                Ok(())
            } else {
                Err(anyhow!(
                    "created semantic model download cache {} has an unexpected filesystem shape",
                    download_cache.display()
                ))
            }
        }
        Err(error) => Err(error).with_context(|| {
            format!(
                "inspect semantic model download cache {}",
                download_cache.display()
            )
        }),
    }
}

#[cfg(ctx_semantic_fastembed)]
fn cleanup_semantic_cpu_download_cache(download_cache: &Path) -> Result<()> {
    match fs::symlink_metadata(download_cache) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            fs::remove_dir_all(download_cache).with_context(|| {
                format!(
                    "remove ctx-managed semantic model download cache {}",
                    download_cache.display()
                )
            })
        }
        Ok(_) => Err(anyhow!(
            "refusing to remove semantic model download cache {} with an unexpected filesystem shape",
            download_cache.display()
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| {
            format!(
                "inspect semantic model download cache {}",
                download_cache.display()
            )
        }),
    }
}

#[cfg(ctx_semantic_fastembed)]
fn stage_semantic_cpu_model_file(
    downloaded: &Path,
    download_cache: &Path,
    destination: &Path,
) -> Result<()> {
    let (source, mut source_file) =
        open_verified_semantic_cpu_model_blob(downloaded, download_cache)?;
    stage_opened_semantic_cpu_model_blob(&source, &mut source_file, destination)
}

#[cfg(ctx_semantic_fastembed)]
fn open_verified_semantic_cpu_model_blob(
    downloaded: &Path,
    download_cache: &Path,
) -> Result<(PathBuf, fs::File)> {
    let canonical_cache = fs::canonicalize(download_cache).with_context(|| {
        format!(
            "resolve semantic model download cache {}",
            download_cache.display()
        )
    })?;
    let source = fs::canonicalize(downloaded).with_context(|| {
        format!(
            "resolve downloaded semantic model file {}",
            downloaded.display()
        )
    })?;
    if !source.starts_with(&canonical_cache) {
        return Err(anyhow!(
            "downloaded semantic model file {} resolves outside ctx-managed cache {}",
            downloaded.display(),
            download_cache.display()
        ));
    }
    let source_file = open_semantic_cpu_model_blob_nofollow(&source)?;
    if !source_file.metadata()?.is_file() {
        return Err(anyhow!(
            "downloaded semantic model blob {} is not a regular file",
            source.display()
        ));
    }
    let source_after_open = fs::canonicalize(&source)
        .with_context(|| format!("re-resolve semantic model blob {}", source.display()))?;
    if source_after_open != source || !source_after_open.starts_with(&canonical_cache) {
        return Err(anyhow!(
            "downloaded semantic model blob {} changed while opening",
            source.display()
        ));
    }
    #[cfg(unix)]
    if !semantic_cpu_model_path_matches_open_file(&source_file, &source)? {
        return Err(anyhow!(
            "downloaded semantic model blob {} changed while opening",
            source.display()
        ));
    }
    Ok((source, source_file))
}

#[cfg(all(ctx_semantic_fastembed, unix))]
fn stage_opened_semantic_cpu_model_blob(
    source: &Path,
    source_file: &mut fs::File,
    destination: &Path,
) -> Result<()> {
    stage_opened_semantic_cpu_model_blob_with_link(
        source,
        source_file,
        destination,
        |source, destination| fs::hard_link(source, destination),
    )
}

#[cfg(all(ctx_semantic_fastembed, unix))]
fn stage_opened_semantic_cpu_model_blob_with_link<F>(
    source: &Path,
    source_file: &mut fs::File,
    destination: &Path,
    hard_link: F,
) -> Result<()>
where
    F: FnOnce(&Path, &Path) -> std::io::Result<()>,
{
    match hard_link(source, destination) {
        Ok(()) => {
            let staged_metadata = fs::symlink_metadata(destination).with_context(|| {
                format!(
                    "inspect hard-linked semantic model file {}",
                    destination.display()
                )
            })?;
            if !staged_metadata.is_file() || staged_metadata.file_type().is_symlink() {
                let _ = fs::remove_file(destination);
                return Err(anyhow!(
                    "hard-linked semantic model file {} is not a regular file",
                    destination.display()
                ));
            }
            let matches_source =
                semantic_cpu_model_path_matches_open_file(source_file, destination).with_context(
                    || {
                        format!(
                            "verify hard-linked semantic model file {}",
                            destination.display()
                        )
                    },
                );
            match matches_source {
                Ok(true) => {}
                Ok(false) => {
                    let _ = fs::remove_file(destination);
                    return Err(anyhow!(
                        "hard-linked semantic model file {} does not match the opened source",
                        destination.display()
                    ));
                }
                Err(error) => {
                    let _ = fs::remove_file(destination);
                    return Err(error);
                }
            }
            Ok(())
        }
        Err(link_error) => {
            copy_opened_semantic_cpu_model_blob(source_file, source, destination, Some(&link_error))
        }
    }
}

#[cfg(all(ctx_semantic_fastembed, windows))]
fn stage_opened_semantic_cpu_model_blob(
    source: &Path,
    source_file: &mut fs::File,
    destination: &Path,
) -> Result<()> {
    copy_opened_semantic_cpu_model_blob(source_file, source, destination, None)
}

#[cfg(all(ctx_semantic_fastembed, not(any(unix, windows))))]
fn stage_opened_semantic_cpu_model_blob(
    _source: &Path,
    _source_file: &mut fs::File,
    _destination: &Path,
) -> Result<()> {
    Err(anyhow!(
        "cannot safely stage semantic model blobs on this platform"
    ))
}

#[cfg(ctx_semantic_fastembed)]
fn copy_opened_semantic_cpu_model_blob(
    source_file: &mut fs::File,
    source: &Path,
    destination: &Path,
    link_error: Option<&std::io::Error>,
) -> Result<()> {
    source_file
        .rewind()
        .with_context(|| format!("rewind semantic model blob {}", source.display()))?;
    let mut destination_file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .with_context(|| match link_error {
            Some(error) => format!(
                "create semantic model staging file {} after hard-link failure: {error}",
                destination.display()
            ),
            None => format!(
                "create semantic model staging file {}",
                destination.display()
            ),
        })?;
    std::io::copy(source_file, &mut destination_file).with_context(|| match link_error {
        Some(error) => format!(
            "copy semantic model blob {} to {} after hard-link failure: {error}",
            source.display(),
            destination.display()
        ),
        None => format!(
            "copy semantic model blob {} to {}",
            source.display(),
            destination.display()
        ),
    })?;
    Ok(())
}

#[cfg(all(ctx_semantic_fastembed, unix))]
fn open_semantic_cpu_model_blob_nofollow(path: &Path) -> Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;

    fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
        .with_context(|| {
            format!(
                "open semantic model blob without following symlinks {}",
                path.display()
            )
        })
}

#[cfg(all(ctx_semantic_fastembed, windows))]
fn open_semantic_cpu_model_blob_nofollow(path: &Path) -> Result<fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;

    fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .with_context(|| {
            format!(
                "open semantic model blob without following reparse points {}",
                path.display()
            )
        })
}

#[cfg(all(ctx_semantic_fastembed, not(any(unix, windows))))]
fn open_semantic_cpu_model_blob_nofollow(path: &Path) -> Result<fs::File> {
    Err(anyhow!(
        "cannot safely open semantic model blob without following links on this platform: {}",
        path.display()
    ))
}

#[cfg(all(ctx_semantic_fastembed, unix))]
fn semantic_cpu_model_path_matches_open_file(opened: &fs::File, path: &Path) -> Result<bool> {
    let current = open_semantic_cpu_model_blob_nofollow(path)?;
    use std::os::unix::fs::MetadataExt;

    let opened = opened.metadata()?;
    let current = current.metadata()?;
    Ok(opened.dev() == current.dev() && opened.ino() == current.ino())
}

#[cfg(ctx_semantic_fastembed)]
fn publish_semantic_cpu_model_root(
    staging_root: &Path,
    model_root: &Path,
    _acquisition_lock: &fs::File,
) -> Result<()> {
    let managed_root = model_root
        .parent()
        .ok_or_else(|| anyhow!("semantic model root has no parent"))?;
    let backup_root = managed_root.join(format!(
        ".{SEMANTIC_HF_MODEL_CACHE_DIR}.backup-{}",
        Uuid::new_v4().simple()
    ));
    let had_previous = model_root.exists();
    if had_previous {
        fs::rename(model_root, &backup_root).with_context(|| {
            format!(
                "preserve previous semantic model cache {}",
                model_root.display()
            )
        })?;
    }
    if let Err(error) = fs::rename(staging_root, model_root) {
        let restore = if had_previous {
            fs::rename(&backup_root, model_root).err()
        } else {
            None
        };
        return Err(anyhow!(match restore {
            Some(restore) => format!(
                "publish semantic model cache {}: {error}; restore previous cache: {restore}",
                model_root.display()
            ),
            None => format!(
                "publish semantic model cache {}: {error}",
                model_root.display()
            ),
        }));
    }
    if had_previous {
        // Publication is already committed. A cleanup failure must not turn a
        // valid model into a retry loop; a later acquisition may remove it.
        let _ = fs::remove_dir_all(&backup_root);
    }
    let _ = cleanup_semantic_cpu_download_cache(&managed_root.join("download-cache"));
    Ok(())
}

#[cfg(ctx_semantic_fastembed)]
pub(super) fn read_semantic_model_file(snapshot: &Path, relative: &str) -> Result<Vec<u8>> {
    let path = snapshot.join(relative);
    fs::read(&path).with_context(|| format!("read semantic model file {}", path.display()))
}

#[cfg(test)]
#[path = "cpu_model_cache_tests.rs"]
mod tests;
