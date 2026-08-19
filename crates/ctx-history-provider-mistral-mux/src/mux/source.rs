use std::{
    fs,
    path::{Path, PathBuf},
};

use ctx_history_provider_runtime::{
    source_io::{
        ensure_provider_path_parents_are_not_symlinks, ensure_regular_provider_transcript_file,
    },
    CaptureError, Result,
};

pub(super) const MUX_MAX_DIRECTORY_DEPTH: usize = 128;
pub(super) const MUX_MAX_TRAVERSAL_ENTRIES: usize = 4_096;
pub(super) const MUX_MAX_SESSION_SOURCES: usize = 4_096;

struct MuxTraversalBudget {
    remaining_entries: usize,
    remaining_sources: usize,
}

impl MuxTraversalBudget {
    fn new(maximum_entries: usize, maximum_sources: usize) -> Self {
        Self {
            remaining_entries: maximum_entries,
            remaining_sources: maximum_sources,
        }
    }

    fn claim_entry(&mut self, path: &Path) -> Result<()> {
        if self.remaining_entries == 0 {
            return Err(CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: "Mux session traversal exceeds the supported directory entry limit",
            });
        }
        self.remaining_entries -= 1;
        Ok(())
    }

    fn claim_source(&mut self, path: &Path) -> Result<()> {
        if self.remaining_sources == 0 {
            return Err(CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: "Mux session traversal exceeds the supported source limit",
            });
        }
        self.remaining_sources -= 1;
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub(super) struct MuxSessionSource {
    pub(super) session_dir: PathBuf,
    pub(super) archive_path: Option<PathBuf>,
    pub(super) chat_path: Option<PathBuf>,
    pub(super) partial_path: Option<PathBuf>,
    pub(super) metadata_path: Option<PathBuf>,
    pub(super) provider_session_id: String,
    pub(super) parent_provider_session_id: Option<String>,
}

pub(super) fn mux_session_source_from_dir(dir: &Path) -> Result<Option<MuxSessionSource>> {
    let archive_path = mux_optional_regular_file(&dir.join("chat-archive.jsonl"))?;
    let chat_path = mux_optional_regular_file(&dir.join("chat.jsonl"))?;
    let partial_path = mux_optional_regular_file(&dir.join("partial.json"))?;
    if archive_path.is_none() && chat_path.is_none() && partial_path.is_none() {
        return Ok(None);
    }
    let metadata_path = mux_optional_regular_file(&dir.join("metadata.json"))?;
    let provider_session_id = dir
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| CaptureError::InvalidProviderTranscriptPath {
            path: dir.to_path_buf(),
            reason: "Mux session directory is missing a workspace id",
        })?;
    let parent_provider_session_id = mux_parent_session_id_from_path(dir);
    Ok(Some(MuxSessionSource {
        session_dir: dir.to_path_buf(),
        archive_path,
        chat_path,
        partial_path,
        metadata_path,
        provider_session_id,
        parent_provider_session_id,
    }))
}

fn mux_optional_regular_file(path: &Path) -> Result<Option<PathBuf>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => {
            ensure_regular_provider_transcript_file(path)?;
            Ok(Some(path.to_path_buf()))
        }
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err(CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: "symlinked provider transcript files are rejected",
            })
        }
        Ok(_) => Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "Mux transcript files must be regular files",
        }),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err.into()),
    }
}

fn mux_parent_session_id_from_path(dir: &Path) -> Option<String> {
    let parent = dir.parent()?;
    if parent.file_name().and_then(|name| name.to_str()) != Some("subagent-transcripts") {
        return None;
    }
    parent
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .map(str::to_owned)
}

pub(super) fn visit_mux_session_sources(
    root: &Path,
    visit: &mut dyn FnMut(MuxSessionSource) -> Result<()>,
) -> Result<usize> {
    let mut budget = MuxTraversalBudget::new(MUX_MAX_TRAVERSAL_ENTRIES, MUX_MAX_SESSION_SOURCES);
    visit_mux_session_sources_bounded(root, visit, &mut budget)
}

#[cfg(test)]
pub(super) fn visit_mux_session_sources_with_limits(
    root: &Path,
    maximum_entries: usize,
    maximum_sources: usize,
    visit: &mut dyn FnMut(MuxSessionSource) -> Result<()>,
) -> Result<usize> {
    let mut budget = MuxTraversalBudget::new(maximum_entries, maximum_sources);
    visit_mux_session_sources_bounded(root, visit, &mut budget)
}

fn visit_mux_session_sources_bounded(
    root: &Path,
    visit: &mut dyn FnMut(MuxSessionSource) -> Result<()>,
    budget: &mut MuxTraversalBudget,
) -> Result<usize> {
    // Complete bounded traversal before exposing any source to inventory
    // accumulation. An over-limit tree therefore fails closed with no partial
    // inventory, while the source vector itself is bounded by claim_source.
    let mut sources = Vec::new();
    visit_mux_session_sources_at_depth(
        root,
        &mut |source| {
            sources.push(source);
            Ok(())
        },
        0,
        budget,
    )?;
    let source_count = sources.len();
    for source in sources {
        visit(source)?;
    }
    Ok(source_count)
}

fn visit_mux_session_sources_at_depth(
    root: &Path,
    visit: &mut dyn FnMut(MuxSessionSource) -> Result<()>,
    depth: usize,
    budget: &mut MuxTraversalBudget,
) -> Result<usize> {
    if depth > MUX_MAX_DIRECTORY_DEPTH {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: root.to_path_buf(),
            reason: "Mux session directory nesting exceeds the supported limit",
        });
    }
    let metadata = fs::symlink_metadata(root)?;
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: root.to_path_buf(),
            reason: "symlinked provider transcript roots are rejected",
        });
    }
    ensure_provider_path_parents_are_not_symlinks(root)?;
    if file_type.is_file() {
        ensure_regular_provider_transcript_file(root)?;
        if matches!(
            root.file_name().and_then(|name| name.to_str()),
            Some("chat-archive.jsonl" | "chat.jsonl" | "partial.json")
        ) {
            if let Some(session_dir) = root.parent() {
                if let Some(source) = mux_session_source_from_dir(session_dir)? {
                    budget.claim_source(session_dir)?;
                    visit(source)?;
                    return Ok(1);
                }
            }
        }
        return Ok(0);
    }
    if !file_type.is_dir() {
        return Ok(0);
    }

    let mut visited = 0_usize;
    if let Some(source) = mux_session_source_from_dir(root)? {
        budget.claim_source(root)?;
        visit(source)?;
        visited = 1;
    }
    let mut directories = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        budget.claim_entry(root)?;
        if entry.file_type()?.is_dir() {
            directories.push(entry);
        }
    }
    directories.sort_unstable_by_key(|entry| entry.file_name());
    for entry in directories {
        visited = visited.saturating_add(visit_mux_session_sources_at_depth(
            &entry.path(),
            visit,
            depth.saturating_add(1),
            budget,
        )?);
    }
    Ok(visited)
}
