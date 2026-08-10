use super::*;

#[cfg(test)]
std::thread_local! {
    static AFTER_CODEX_METADATA_INVENTORY_HOOK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn install_after_codex_metadata_inventory_hook(hook: impl FnOnce() + 'static) {
    AFTER_CODEX_METADATA_INVENTORY_HOOK.with(|slot| {
        assert!(
            slot.borrow().is_none(),
            "Codex metadata-inventory hook is already installed"
        );
        *slot.borrow_mut() = Some(Box::new(hook));
    });
}

#[cfg(test)]
fn run_after_codex_metadata_inventory_hook() {
    let hook = AFTER_CODEX_METADATA_INVENTORY_HOOK.with(|slot| slot.borrow_mut().take());
    if let Some(hook) = hook {
        hook();
    }
}

#[derive(Debug, Clone)]
pub(crate) struct CodexSessionTreeInventoryV0 {
    pub(crate) sources: Vec<(CodexCatalogSource, SourceKey, String)>,
    #[cfg(test)]
    pub(crate) work: CodexCatalogWorkV0,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct CodexCatalogWorkV0 {
    pub(crate) inventory_walks: u64,
    pub(crate) source_observations: u64,
    #[cfg(test)]
    pub(crate) source_hash_reads: u64,
    pub(crate) source_metadata_opens: u64,
    pub(crate) source_metadata_read_upper_bound_bytes: u64,
    pub(crate) session_meta_parses: u64,
}

impl CodexCatalogWorkV0 {
    fn add_assign(&mut self, other: Self) {
        self.inventory_walks = self.inventory_walks.saturating_add(other.inventory_walks);
        self.source_observations = self
            .source_observations
            .saturating_add(other.source_observations);
        #[cfg(test)]
        {
            self.source_hash_reads = self
                .source_hash_reads
                .saturating_add(other.source_hash_reads);
        }
        self.source_metadata_opens = self
            .source_metadata_opens
            .saturating_add(other.source_metadata_opens);
        self.source_metadata_read_upper_bound_bytes = self
            .source_metadata_read_upper_bound_bytes
            .saturating_add(other.source_metadata_read_upper_bound_bytes);
        self.session_meta_parses = self
            .session_meta_parses
            .saturating_add(other.session_meta_parses);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CodexExplicitSessionSourceBackedInputV0 {
    path: PathBuf,
    source: SourceKey,
    native_session_id: String,
}

impl CodexExplicitSessionSourceBackedInputV0 {
    pub(crate) fn discover(path: impl AsRef<Path>) -> CodexSourceBackedResultV0<Self> {
        let path = absolute_lexical_path(path.as_ref())?;
        let (_, source, native_session_id) = open_codex_explicit_source_plan_v0(&path)?;
        Ok(Self {
            path,
            source,
            native_session_id,
        })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn source(&self) -> &SourceKey {
        &self.source
    }
}

// The 488-byte present plan is moved intact through bounded inventory discovery;
// boxing it would add allocation without reducing retained source authority.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
enum CodexExplicitSessionInventoryStateV0 {
    Present {
        plan: (CodexCatalogSource, SourceKey, String),
    },
    Missing,
}

impl PartialEq for CodexExplicitSessionInventoryStateV0 {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Present { plan: left }, Self::Present { plan: right }) => {
                let (left_source, left_key, left_native_session_id) = left;
                let (right_source, right_key, right_native_session_id) = right;
                left_key.exact_descriptor_eq(right_key)
                    && left_native_session_id == right_native_session_id
                    && left_source.source_root == right_source.source_root
                    && left_source.source_path == right_source.source_path
                    && left_source.catalog_native_session_id
                        == right_source.catalog_native_session_id
                    && left_source.catalog_parent_native_session_id
                        == right_source.catalog_parent_native_session_id
                    && left_source.catalog_session_relationship
                        == right_source.catalog_session_relationship
                    && left_source.catalog_advisory_session_id
                        == right_source.catalog_advisory_session_id
                    && left_source.catalog_root_native_session_id
                        == right_source.catalog_root_native_session_id
            }
            (Self::Missing, Self::Missing) => true,
            _ => false,
        }
    }
}

impl Eq for CodexExplicitSessionInventoryStateV0 {}

/// One finite observation of exactly one caller-selected Codex rollout.
#[derive(Debug, Clone)]
pub(crate) struct CodexExplicitSessionInventoryV0 {
    state: CodexExplicitSessionInventoryStateV0,
}

impl CodexExplicitSessionInventoryV0 {
    pub(crate) fn source_plan(&self) -> Option<(CodexCatalogSource, SourceKey, String)> {
        match &self.state {
            CodexExplicitSessionInventoryStateV0::Present { plan } => Some(plan.clone()),
            CodexExplicitSessionInventoryStateV0::Missing => None,
        }
    }
}

pub(crate) fn observe_codex_explicit_session_source_backed_v0(
    input: &CodexExplicitSessionSourceBackedInputV0,
) -> CodexSourceBackedResultV0<CodexExplicitSessionInventoryV0> {
    let state = match open_codex_explicit_source_plan_v0(input.path()) {
        Ok(plan)
            if plan.1.exact_descriptor_eq(input.source()) && plan.2 == input.native_session_id =>
        {
            CodexExplicitSessionInventoryStateV0::Present { plan }
        }
        Ok(_) => return Err(CodexSourceBackedErrorV0::ExplicitSourceIdentityChanged),
        Err(CodexSourceBackedErrorV0::Capture(CaptureError::Io(error)))
            if error.kind() == std::io::ErrorKind::NotFound =>
        {
            CodexExplicitSessionInventoryStateV0::Missing
        }
        Err(error) => return Err(error),
    };
    Ok(CodexExplicitSessionInventoryV0 { state })
}

fn open_codex_explicit_source_plan_v0(
    path: &Path,
) -> CodexSourceBackedResultV0<(CodexCatalogSource, SourceKey, String)> {
    let opened = Arc::new(open_provider_source_file(path)?);
    let frozen_observation = opened_codex_file_observation(path, opened.file())?;
    let frozen_prefix_sha256 = opened_file_prefix_sha256(opened.file(), frozen_observation.len)?;
    let after = opened_codex_file_observation(path, opened.file())?;
    if !frozen_observation.admits_append_only_growth(&after) {
        return Err(CodexSourceBackedErrorV0::Capture(
            CaptureError::SourceChangedDuringCapture,
        ));
    }
    let catalog = catalog_codex_explicit_session_opened(path, &opened)?;
    let discovery = super::discover_codex_catalog_sources(&[catalog]);
    if discovery.ineligible != 0 || !discovery.rejections.is_empty() {
        return Err(CodexSourceBackedErrorV0::IncompleteCatalog {
            rejected: discovery.rejections.len(),
            failed: discovery.ineligible,
        });
    }
    let mut sources = discovery.sources;
    let Some(source) = sources.first_mut() else {
        return Err(CodexSourceBackedErrorV0::IncompleteCatalog {
            rejected: 1,
            failed: 0,
        });
    };
    if !frozen_observation.admits_append_only_growth(&source.catalog_observation) {
        return Err(CodexSourceBackedErrorV0::Capture(
            CaptureError::SourceChangedDuringCapture,
        ));
    }
    source.catalog_observation = frozen_observation;
    source.catalog_prefix_sha256 = Some(frozen_prefix_sha256);
    source.opened = Some(opened);
    let mut bound = bind_source_keys(sources)?;
    if bound.len() != 1 {
        return Err(CodexSourceBackedErrorV0::IncompleteCatalog {
            rejected: bound.len(),
            failed: 0,
        });
    }
    bound
        .pop()
        .ok_or(CodexSourceBackedErrorV0::IncompleteCatalog {
            rejected: 1,
            failed: 0,
        })
}

fn absolute_lexical_path(path: &Path) -> CodexSourceBackedResultV0<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            _ => normalized.push(component.as_os_str()),
        }
    }
    Ok(normalized)
}

pub(super) fn bind_source_keys(
    sources: Vec<CodexCatalogSource>,
) -> CodexSourceBackedResultV0<Vec<(CodexCatalogSource, SourceKey, String)>> {
    let mut bound = Vec::with_capacity(sources.len());
    for source in sources {
        let native_session_id = source.catalog_native_session_id.clone().ok_or_else(|| {
            CodexSourceBackedErrorV0::MissingNativeSessionId {
                path: source.source_path.clone(),
            }
        })?;
        let source_key = codex_source_key(&native_session_id)?;
        bound.push((source, source_key, native_session_id));
    }
    Ok(bound)
}

pub(crate) fn discover_codex_session_tree_inventory_v0(
    session_roots: &[PathBuf],
) -> CodexSourceBackedResultV0<CodexSessionTreeInventoryV0> {
    discover_codex_deferred_session_tree_inventory_v0(session_roots)
}

/// Discovers stable leaf identities and observations without opening provider
/// bodies. The JSONL family later supplies each leaf's durable base
/// certificate, so only a changed or parser-migrated leaf needs hydration from
/// its own bytes.
pub(crate) fn discover_codex_deferred_session_tree_inventory_v0(
    session_roots: &[PathBuf],
) -> CodexSourceBackedResultV0<CodexSessionTreeInventoryV0> {
    discover_codex_session_tree_metadata_inventory_v0(session_roots)
}

#[derive(Debug)]
struct CodexMetadataInventoryLeafV0 {
    source_root: String,
    source_path: PathBuf,
    relative_path: PathBuf,
    observation: CodexFileObservation,
    authority: ProviderSourceRoot,
}

fn discover_codex_session_tree_metadata_inventory_v0(
    session_roots: &[PathBuf],
) -> CodexSourceBackedResultV0<CodexSessionTreeInventoryV0> {
    let normalized_roots = normalized_session_roots(session_roots)?;
    let mut leaves = Vec::new();
    let mut authorities = Vec::with_capacity(normalized_roots.len());
    let mut work = CodexCatalogWorkV0::default();
    for session_root in &normalized_roots {
        let (root, mut root_leaves, root_work) =
            discover_codex_metadata_inventory_root_v0(session_root)?;
        work.add_assign(root_work);
        crate::provider::codex::catalog::ensure_catalog_source_bound(
            leaves.len().saturating_add(root_leaves.len()),
        )?;
        leaves.append(&mut root_leaves);
        authorities.push(root);
    }

    #[cfg(test)]
    run_after_codex_metadata_inventory_hook();

    let mut catalog_sources = Vec::with_capacity(leaves.len());
    for leaf in leaves {
        catalog_sources.push(catalog_source_from_path_hint(&leaf, &mut work)?);
    }
    for authority in &authorities {
        authority.revalidate()?;
    }

    let mut sources = bind_source_keys(catalog_sources)?;
    sort_bound_sources(&mut sources);
    Ok(CodexSessionTreeInventoryV0 {
        sources,
        #[cfg(test)]
        work,
    })
}

fn discover_codex_metadata_inventory_root_v0(
    session_root: &Path,
) -> CodexSourceBackedResultV0<(
    ProviderSourceRoot,
    Vec<CodexMetadataInventoryLeafV0>,
    CodexCatalogWorkV0,
)> {
    let authority = ProviderSourceRoot::open(session_root)?;
    let mut leaves = Vec::new();
    let mut pending = vec![(PathBuf::new(), 0_usize)];
    let mut directory_observations = Vec::new();
    let mut visited_directories = 0_usize;
    let mut visited_entries = 0_usize;
    while let Some((relative_directory, depth)) = pending.pop() {
        if depth > PROVIDER_JSONL_INVENTORY_MAX_DEPTH {
            return Err(CodexSourceBackedErrorV0::Capture(
                CaptureError::InvalidPayload(
                    "Codex catalog directory depth exceeds the provider inventory bound".to_owned(),
                ),
            ));
        }
        visited_directories = visited_directories.saturating_add(1);
        if visited_directories > PROVIDER_JSONL_INVENTORY_MAX_DIRECTORIES {
            return Err(CodexSourceBackedErrorV0::Capture(
                CaptureError::InvalidPayload(
                    "Codex catalog directory count exceeds the provider inventory bound".to_owned(),
                ),
            ));
        }
        let directory = authority.open_directory(&relative_directory)?;
        let names = directory.entries(
            PROVIDER_JSONL_INVENTORY_MAX_METADATA_ENTRIES
                .saturating_sub(visited_entries)
                .saturating_add(1),
        )?;
        visited_entries = visited_entries.saturating_add(names.len());
        if visited_entries > PROVIDER_JSONL_INVENTORY_MAX_METADATA_ENTRIES {
            return Err(CodexSourceBackedErrorV0::Capture(
                CaptureError::InvalidPayload(
                    "Codex catalog entry count exceeds the provider inventory bound".to_owned(),
                ),
            ));
        }
        let mut child_directories = Vec::new();
        for name in names {
            let relative_path = relative_directory.join(&name);
            let source_path = session_root.join(&relative_path);
            if source_path.as_os_str().as_encoded_bytes().len()
                > PROVIDER_JSONL_INVENTORY_MAX_PATH_BYTES
            {
                return Err(CodexSourceBackedErrorV0::Capture(
                    CaptureError::InvalidPayload(
                        "Codex catalog path exceeds the provider inventory bound".to_owned(),
                    ),
                ));
            }
            match directory.open_child(&name)? {
                OpenedProviderSourcePath::Directory(_) => {
                    child_directories.push((relative_path, depth.saturating_add(1)));
                }
                OpenedProviderSourcePath::File(opened)
                    if source_path
                        .extension()
                        .and_then(|extension| extension.to_str())
                        == Some("jsonl") =>
                {
                    crate::provider::provider_path_identity(&source_path)?;
                    let observation = opened_codex_file_observation(&source_path, opened.file())?;
                    let after = opened_codex_file_observation(&source_path, opened.file())?;
                    if !observation.admits_append_only_growth(&after) {
                        return Err(CodexSourceBackedErrorV0::Capture(
                            CaptureError::SourceChangedDuringCapture,
                        ));
                    }
                    opened.revalidate_leaf()?;
                    leaves.push(CodexMetadataInventoryLeafV0 {
                        source_root: session_root.display().to_string(),
                        source_path,
                        relative_path,
                        observation,
                        authority: authority.clone(),
                    });
                    crate::provider::codex::catalog::ensure_catalog_source_bound(leaves.len())?;
                }
                OpenedProviderSourcePath::File(_) => {}
            }
        }
        directory.revalidate()?;
        directory_observations.push((
            relative_directory.clone(),
            directory.authority_fingerprint(),
        ));
        child_directories.reverse();
        pending.extend(child_directories);
    }
    // Reopen every visited directory after the complete walk and compare its
    // exact metadata stamp. This bounded second pass catches a nested source
    // that reappears after its directory was enumerated without retaining up
    // to 32,768 directory descriptors for the duration of discovery.
    for (relative_directory, expected) in directory_observations {
        let current = authority.open_directory(&relative_directory)?;
        if current.authority_fingerprint() != expected {
            return Err(CodexSourceBackedErrorV0::Capture(
                CaptureError::SourceChangedDuringCapture,
            ));
        }
        current.revalidate()?;
    }
    authority.revalidate()?;
    let source_observations =
        u64::try_from(leaves.len()).map_err(|_| CodexSourceBackedErrorV0::CountOverflow)?;
    Ok((
        authority,
        leaves,
        CodexCatalogWorkV0 {
            inventory_walks: 1,
            source_observations,
            ..CodexCatalogWorkV0::default()
        },
    ))
}

fn catalog_source_from_path_hint(
    leaf: &CodexMetadataInventoryLeafV0,
    work: &mut CodexCatalogWorkV0,
) -> CodexSourceBackedResultV0<CodexCatalogSource> {
    let native_session_id = match codex_canonical_native_session_id_path_hint(&leaf.source_path) {
        Some(native_session_id) => native_session_id,
        None => {
            let opened = leaf.authority.open_file(&leaf.relative_path)?;
            let admitted = opened_codex_file_observation(&leaf.source_path, opened.file())?;
            if !leaf.observation.admits_append_only_growth(&admitted) {
                return Err(CodexSourceBackedErrorV0::Capture(
                    CaptureError::SourceChangedDuringCapture,
                ));
            }
            let native_session_id =
                crate::provider::codex::catalog::probe_codex_native_session_id(&opened)?
                    .or_else(|| codex_native_session_id_path_hint(&leaf.source_path));
            opened.revalidate_leaf()?;
            work.add_assign(CodexCatalogWorkV0 {
                source_metadata_opens: 1,
                source_metadata_read_upper_bound_bytes: leaf.observation.len,
                session_meta_parses: 1,
                ..CodexCatalogWorkV0::default()
            });
            native_session_id.ok_or_else(|| CodexSourceBackedErrorV0::MissingNativeSessionId {
                path: leaf.source_path.clone(),
            })?
        }
    };
    Ok(CodexCatalogSource {
        source_root: leaf.source_root.clone(),
        source_path: leaf.source_path.clone(),
        cataloged_at_ms: 0,
        catalog_observation: leaf.observation.clone(),
        catalog_prefix_sha256: None,
        catalog_native_session_id: Some(native_session_id.clone()),
        catalog_parent_native_session_id: None,
        catalog_session_relationship: SessionRelationshipKind::Root,
        catalog_advisory_session_id: None,
        catalog_root_native_session_id: Some(native_session_id),
        opened: None,
        authority_root: Some(leaf.authority.clone()),
        authority_relative_path: Some(leaf.relative_path.clone()),
    })
}

pub(super) fn set_child_local_root(
    source: &mut CodexCatalogSource,
    native_session_id: &str,
) -> CodexSourceBackedResultV0<()> {
    // A non-root source can certify only the direct-parent claim present in
    // its own bytes. Resolving a generation-wide root would make the claim
    // depend on another source and would reintroduce ancestor participation.
    source.catalog_root_native_session_id = match source.catalog_session_relationship {
        SessionRelationshipKind::Root => Some(native_session_id.to_owned()),
        _ => Some(
            source
                .catalog_parent_native_session_id
                .clone()
                .ok_or(CodexSourceBackedErrorV0::InvalidCheckpoint)?,
        ),
    };
    Ok(())
}

fn catalog_source_from_body(
    leaf: &CodexMetadataInventoryLeafV0,
) -> CodexSourceBackedResultV0<(CodexCatalogSource, bool)> {
    let opened = leaf.authority.open_file(&leaf.relative_path)?;
    let admitted = opened_codex_file_observation(&leaf.source_path, opened.file())?;
    opened.revalidate_leaf()?;
    if !leaf.observation.admits_append_only_growth(&admitted) {
        return Err(CodexSourceBackedErrorV0::Capture(
            CaptureError::SourceChangedDuringCapture,
        ));
    }
    let prefix_sha256 = opened_file_prefix_sha256(opened.file(), leaf.observation.len)?;
    // The catalog helper revalidates this retained authority before and after
    // its bounded session-meta read, so a mutation in that window fails closed.
    let mut catalog = catalog_codex_explicit_session_opened(&leaf.source_path, &opened)?;
    catalog.source_root = leaf.source_root.clone();
    let discovery = super::discover_codex_catalog_sources(&[catalog]);
    if discovery.ineligible != 0 || !discovery.rejections.is_empty() {
        return Err(CodexSourceBackedErrorV0::IncompleteCatalog {
            rejected: discovery.rejections.len(),
            failed: discovery.ineligible,
        });
    }
    let mut sources = discovery.sources;
    if sources.len() != 1 {
        return Err(CodexSourceBackedErrorV0::IncompleteCatalog {
            rejected: sources.len(),
            failed: 0,
        });
    }
    let mut source = sources
        .pop()
        .ok_or(CodexSourceBackedErrorV0::IncompleteCatalog {
            rejected: 1,
            failed: 0,
        })?;
    // Metadata discovery froze this refresh's EOF. Prove those exact bytes on
    // the retained ordinary-file authority before handing them to the parser;
    // growth is intentionally deferred to the next refresh.
    if !leaf
        .observation
        .admits_append_only_growth(&source.catalog_observation)
    {
        return Err(CodexSourceBackedErrorV0::Capture(
            CaptureError::SourceChangedDuringCapture,
        ));
    }
    source.catalog_prefix_sha256 = Some(prefix_sha256);
    source.catalog_observation = leaf.observation.clone();
    source.authority_root = Some(leaf.authority.clone());
    source.authority_relative_path = Some(leaf.relative_path.clone());
    Ok((source, true))
}

pub(super) fn hydrate_codex_session_plan_v0(
    plan: (CodexCatalogSource, SourceKey, String),
    base: Option<&CertifiedSource>,
) -> CodexSourceBackedResultV0<(
    (CodexCatalogSource, SourceKey, String),
    CodexCatalogWorkV0,
    bool,
)> {
    let (mut source, source_key, native_session_id) = plan;
    if let Some(proof) = base
        .filter(|base| base.parser_revision() == CODEX_PARSER_REVISION)
        .and_then(|base| decode_append_proof(&source, &source_key, base).ok())
        .filter(|proof| proof.checkpoint.observation == source.catalog_observation)
    {
        if proof.checkpoint.owner.native_session_id != native_session_id {
            return Err(CodexSourceBackedErrorV0::InvalidCheckpoint);
        }
        source.catalog_prefix_sha256 = Some(proof.checkpoint.full_revision_sha256);
        source.catalog_parent_native_session_id =
            proof.checkpoint.owner.parent_native_session_id.clone();
        source.catalog_session_relationship = proof.checkpoint.owner.session_relationship;
        source.catalog_advisory_session_id = proof.checkpoint.owner.advisory_session_id.clone();
        set_child_local_root(&mut source, &native_session_id)?;
        return Ok((
            (source, source_key, native_session_id),
            CodexCatalogWorkV0::default(),
            true,
        ));
    }

    if source.catalog_prefix_sha256.is_some() {
        set_child_local_root(&mut source, &native_session_id)?;
        return Ok((
            (source, source_key, native_session_id),
            CodexCatalogWorkV0::default(),
            false,
        ));
    }

    let authority = source
        .authority_root
        .clone()
        .ok_or(CodexSourceBackedErrorV0::Capture(
            CaptureError::SystemInvariant("Codex deferred source has no retained authority root"),
        ))?;
    let relative_path =
        source
            .authority_relative_path
            .clone()
            .ok_or(CodexSourceBackedErrorV0::Capture(
                CaptureError::SystemInvariant(
                    "Codex deferred source has no retained authority path",
                ),
            ))?;
    let leaf = CodexMetadataInventoryLeafV0 {
        source_root: source.source_root.clone(),
        source_path: source.source_path.clone(),
        relative_path,
        observation: source.catalog_observation.clone(),
        authority,
    };
    let (mut hydrated, _) = catalog_source_from_body(&leaf)?;
    if hydrated.catalog_native_session_id.as_deref() != Some(native_session_id.as_str()) {
        return Err(CodexSourceBackedErrorV0::ExplicitSourceIdentityChanged);
    }
    set_child_local_root(&mut hydrated, &native_session_id)?;
    Ok((
        (hydrated, source_key, native_session_id),
        CodexCatalogWorkV0 {
            source_metadata_opens: 1,
            // The catalog helper reads only its bounded session-meta prefix;
            // the frozen source length is a conservative read upper bound,
            // not a claim that the transcript body was scanned.
            source_metadata_read_upper_bound_bytes: leaf.observation.len,
            session_meta_parses: 1,
            #[cfg(test)]
            source_hash_reads: 1,
            ..CodexCatalogWorkV0::default()
        },
        false,
    ))
}

fn codex_canonical_native_session_id_path_hint(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    if stem.len() >= 36 {
        let tail = &stem[stem.len() - 36..];
        if tail
            .chars()
            .all(|character| character.is_ascii_hexdigit() || character == '-')
        {
            return Some(tail.to_owned());
        }
    }
    None
}

pub(super) fn codex_native_session_id_path_hint(path: &Path) -> Option<String> {
    if let Some(native_session_id) = codex_canonical_native_session_id_path_hint(path) {
        return Some(native_session_id);
    }
    let stem = path.file_stem()?.to_str()?;
    (!stem.trim().is_empty()).then(|| stem.to_owned())
}

pub(super) fn codex_terminal_native_session_id_hint(
    path: &Path,
    authority: &ProviderSourceRoot,
    authority_path: &Path,
) -> CodexSourceBackedResultV0<Option<String>> {
    let opened = authority.open_file(authority_path)?;
    Ok(
        crate::provider::codex::catalog::probe_codex_native_session_id(&opened)?
            .or_else(|| codex_native_session_id_path_hint(path)),
    )
}

fn normalized_session_roots(session_roots: &[PathBuf]) -> CodexSourceBackedResultV0<Vec<PathBuf>> {
    let mut normalized_roots = session_roots
        .iter()
        .map(|root| absolute_lexical_path(root))
        .collect::<CodexSourceBackedResultV0<Vec<_>>>()?;
    normalized_roots.sort();
    normalized_roots.dedup();
    if normalized_roots.is_empty() {
        return Err(CodexSourceBackedErrorV0::Capture(
            CaptureError::InvalidPayload(
                "Codex session-tree authority has no inventory roots".to_owned(),
            ),
        ));
    }
    Ok(normalized_roots)
}

fn sort_bound_sources(sources: &mut [(CodexCatalogSource, SourceKey, String)]) {
    sources.sort_by(|left, right| {
        left.1
            .identity()
            .digest()
            .cmp(&right.1.identity().digest())
            .then_with(|| {
                left.1
                    .exact_descriptor_digest()
                    .cmp(&right.1.exact_descriptor_digest())
            })
            .then_with(|| left.2.cmp(&right.2))
    });
}
