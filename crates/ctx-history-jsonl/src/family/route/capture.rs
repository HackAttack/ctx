use super::*;

pub(super) fn default_base_source_path<R: JsonlFamilyRuntime>(
    _adapter: &(impl JsonlFamilyAdapter<Runtime = R> + ?Sized),
    certificate: &CertifiedSource,
) -> JsonlResult<PathBuf, JsonlRuntimeError<R>> {
    certificate
        .validate_contract()
        .map_err(contract_error::<JsonlRuntimeError<R>>)?;
    // Parser revisions govern projection semantics, not source ownership. The
    // family still needs the prior source path so an unchanged source can be
    // selected and replaced under the current parser rather than rejected.
    let frontier = certificate.frontier().ok_or_else(|| {
        JsonlRuntimeError::<R>::invalid_payload("JSONL base frontier is absent".to_owned())
    })?;
    // This lookup recovers ownership only; it does not authorize continuation.
    // Decode a structurally current checkpoint even when its outer kind is a
    // retired value so the source can be selected for a conservative full
    // replacement. `decode_checkpoint` separately requires the current kind
    // before granting no-op or suffix authority.
    let checkpoint =
        FamilyCheckpoint::decode_frontier_key::<JsonlRuntimeError<R>>(frontier.checkpoint())?;
    if checkpoint.physical.identity().source_descriptor_digest()
        != &certificate.observation().source().exact_descriptor_digest()
    {
        return Err(JsonlRuntimeError::<R>::invalid_payload(
            "JSONL base checkpoint source changed".to_owned(),
        ));
    }
    Ok(checkpoint.physical.identity().source_path().clone())
}

pub fn jsonl_family_driver<R: JsonlFamilyRuntime>(
    adapter: Arc<dyn JsonlFamilyAdapter<Runtime = R>>,
    root: PathBuf,
) -> super::super::JsonlRuntimeDriver<R> {
    let resident = Arc::new(Mutex::new(FamilyResident::<JsonlRuntimeError<R>>::default()));
    let scan_adapter = Arc::clone(&adapter);
    let scan_root = root.clone();
    let scan_resident = Arc::clone(&resident);
    let owns_adapter = Arc::clone(&adapter);
    let owns_resident = Arc::clone(&resident);
    let revalidation_resident = Arc::clone(&resident);
    let terminal_adapter = adapter;
    let terminal_root = root;
    let inventory_resident = Arc::clone(&resident);

    super::super::JsonlRuntimeDriver::<R>::new(
        move |sink| capture(&*scan_adapter, &scan_root, &scan_resident, sink),
        move |source| {
            owns_adapter.owns(source)
                && owns_resident.lock().is_ok_and(|resident| {
                    !resident.ownership_initialized
                        || resident
                            .owned_sources
                            .get(&source.exact_descriptor_digest())
                            .is_some_and(|owned| owned.exact_descriptor_eq(source))
                })
        },
        move |target| revalidate_target(&revalidation_resident, target),
    )
    .with_parallel_leaf_workers()
    .with_fallible_complete_inventory_revalidation(move |expected| {
        match revalidate_complete_inventory(
            terminal_adapter.as_ref(),
            &terminal_root,
            &inventory_resident,
            expected,
        ) {
            Ok(revalidated) => Ok(revalidated),
            Err(error)
                if normalized_jsonl_error_kind(&error)
                    .unwrap_or_else(|| terminal_adapter.scan_error_kind(&error))
                    == SourceBackedRouteErrorKind::SourceChanged =>
            {
                Ok(false)
            }
            Err(error) => Err(route_scan(terminal_adapter.as_ref(), error)),
        }
    })
}

pub(super) fn capture<R: JsonlFamilyRuntime>(
    adapter: &dyn JsonlFamilyAdapter<Runtime = R>,
    root: &Path,
    resident: &Mutex<FamilyResident<JsonlRuntimeError<R>>>,
    sink: &mut SourceBackedGenerationSink<'_, R::Lifecycle>,
) -> SourceBackedRouteResult<()> {
    if let Some(members) = sink.member_workset().cloned() {
        let partial_captured = capture_partial_members(adapter, root, resident, sink, &members)?;
        if partial_captured {
            return Ok(());
        }
        adapter
            .prepare_partial_member_fallback()
            .map_err(|error| route_discovery(adapter, error))?;
    }
    reset_terminal(resident)?;
    let opening = adapter
        .discover(root)
        .map_err(|error| route_discovery(adapter, error))?;
    let opening_membership = adapter
        .observe_terminal_membership(root, &opening)
        .map_err(|error| route_discovery(adapter, error))?;
    if opening.root_missing()
        && adapter.root_missing_mode() == JsonlFamilyRootMissingMode::Unavailable
    {
        return Err(SourceBackedRouteError::new(
            SourceBackedRouteErrorKind::Unavailable,
            "provider JSONL root is unavailable",
        ));
    }
    if opening.leaves().is_empty() && !opening.rejected_leaves().is_empty() {
        let rejected_records =
            opening
                .rejected_leaves()
                .iter()
                .try_fold(0_u64, |total, leaf| {
                    total.checked_add(leaf.rejected_records).ok_or_else(|| {
                        SourceBackedRouteError::new(
                            SourceBackedRouteErrorKind::Internal,
                            "provider JSONL rejected-record count overflow",
                        )
                    })
                })?;
        return Err(SourceBackedRouteError::new(
            SourceBackedRouteErrorKind::InvalidSource,
            format!(
                "direct JSONL route rejected {rejected_records} records across {} sources; \
                 all provider-native session identity leaves were rejected",
                opening.rejected_leaves().len(),
            ),
        ));
    }
    let bases = base_sources_for_root(adapter, &opening, root, sink)?;
    let mut selected_leaves = opening
        .leaves()
        .iter()
        .filter(|leaf| {
            adapter.base_scope() == JsonlFamilyBaseScope::ProviderFamily
                || !sink.source_owned_by_other_route(leaf.source())
        })
        .cloned()
        .collect::<Vec<_>>();
    adapter
        .order_leaf_scans(&mut selected_leaves)
        .map_err(|error| route_scan(adapter, error))?;
    let exact_scan_total_bytes = selected_leaves.iter().try_fold(0_u64, |total, leaf| {
        total.checked_add(leaf.frozen_scan_observation()?.length())
    });
    // Only leaves whose existing capture contract already freezes the opening
    // observation opt in. Overflow or any other family shape simply abstains.
    if let Some(total) = exact_scan_total_bytes {
        sink.enable_exact_scan_accounting(total);
        if selected_leaves.is_empty() {
            sink.report_completed_bytes_with_exact(0, Some(0))
                .map_err(route_internal)?;
        }
    }
    let mut owned_sources = HashMap::with_capacity(bases.len() + selected_leaves.len());
    for source in bases
        .iter()
        .map(|base| base.observation().source())
        .chain(selected_leaves.iter().map(JsonlFamilyLeaf::source))
    {
        let digest = source.exact_descriptor_digest();
        if owned_sources
            .insert(digest, source.clone())
            .is_some_and(|previous| !previous.exact_descriptor_eq(source))
        {
            return Err(route_invalid(
                "JSONL route source descriptor digest collision",
            ));
        }
    }
    let bases_by_descriptor = bases_by_descriptor(&bases)?;
    let base_event_lookup = sink.base_event_lookup();
    let mut scan_selected_leaves = Vec::with_capacity(selected_leaves.len());
    let mut retained_terminal_sources = HashMap::new();
    #[cfg(test)]
    tests::begin_admission(selected_leaves.len(), bases.len());
    let append_only_trust_allowed = sink.reconciliation_demand()
        == ctx_history_capture_runtime::SourceBackedReconciliationDemand::Incremental;
    for leaf in &selected_leaves {
        let Some(base) = base_for_leaf(&bases_by_descriptor, leaf) else {
            scan_selected_leaves.push(leaf.clone());
            continue;
        };
        let Ok(observation) =
            source_observation::<JsonlRuntimeError<R>>(leaf.source(), leaf.observation())
        else {
            scan_selected_leaves.push(leaf.clone());
            continue;
        };
        if observation != *base.observation() {
            scan_selected_leaves.push(leaf.clone());
            continue;
        }
        #[cfg(test)]
        let decoded = decode_checkpoint(adapter, leaf, base)
            .inspect_err(|_| tests::record_checkpoint_rejection());
        #[cfg(not(test))]
        let decoded = decode_checkpoint(adapter, leaf, base);
        let Ok(checkpoint) = decoded else {
            scan_selected_leaves.push(leaf.clone());
            continue;
        };
        if !checkpoint.physical.terminal() {
            scan_selected_leaves.push(leaf.clone());
            continue;
        }
        if !append_only_trust_allowed
            && adapter.append_trust_contract()
                == JsonlFamilyAppendTrustContract::AppendOnlySameObjectV1
        {
            scan_selected_leaves.push(leaf.clone());
            continue;
        }
        let terminal_proof = JsonlFamilyTerminalProof::unchanged(adapter, leaf, base, &checkpoint)
            .map_err(|error| route_scan(adapter, error))?;
        sink.retain_source(base.clone()).map_err(route_internal)?;
        sink.report_completed_bytes_with_exact(
            base.counts().certified_bytes,
            leaf.frozen_scan_observation()
                .map(|observation| observation.length()),
        )
        .map_err(route_internal)?;
        retained_terminal_sources.insert(
            leaf.source().exact_descriptor_digest(),
            TerminalSourceEvidence {
                certificate: base.clone(),
                terminal_proof,
                emitted_bytes: 0,
                exact_scan_bytes: leaf
                    .frozen_scan_observation()
                    .map(|observation| observation.length()),
                record_rejections: SourceBackedRecordRejectionDrafts::default(),
            },
        );
    }
    #[cfg(test)]
    tests::record_retained_sources(retained_terminal_sources.len());
    let terminal_sources = scan_leaves(
        adapter,
        &scan_selected_leaves,
        &bases_by_descriptor,
        base_event_lookup,
        sink,
        append_only_trust_allowed,
    );
    let finish_leaf_scans = adapter
        .finish_leaf_scans()
        .map_err(|error| route_scan(adapter, error));
    let mut terminal_sources = terminal_sources?;
    finish_leaf_scans?;
    for (digest, evidence) in retained_terminal_sources {
        if terminal_sources.insert(digest, evidence).is_some() {
            return Err(route_invalid("duplicate JSONL terminal source evidence"));
        }
    }

    let selected_sources = selected_leaves
        .iter()
        .map(|leaf| leaf.source().clone())
        .collect::<Vec<_>>();
    let inventory = opening
        .certify_selected_against(&opening, selected_sources)
        .map_err(route_invalid)?;
    sink.certify_complete_inventory(inventory.clone())
        .map_err(route_internal)?;
    let mut absent_sources = Vec::new();
    for base in &bases {
        if !inventory.contains(base.observation().source()) {
            if let Some(absent) = JsonlFamilyAbsentMember::from_path(
                &opening,
                adapter
                    .base_source_path(base)
                    .map_err(|error| route_scan(adapter, error))?,
            ) {
                absent_sources.push(absent);
            }
            let deletion = CertifiedSourceDeletion::from_inventory(
                base.observation().source().clone(),
                &inventory,
            )
            .map_err(route_invalid)?;
            sink.delete_source(deletion, inventory.clone())
                .map_err(route_internal)?;
        }
    }
    let mut resident = resident
        .lock()
        .map_err(|_| route_internal("JSONL resident catalog lock was poisoned"))?;
    resident.ownership_initialized = true;
    resident.owned_sources = owned_sources;
    resident.terminal_sources = terminal_sources;
    resident.absent_sources = absent_sources;
    resident.opening_membership = Some(opening_membership);
    resident.certified_inventory = Some(inventory);
    resident.opening_inventory = Some(opening);
    Ok(())
}

/// Attempts one bounded existing-member refresh without enumerating route
/// membership. `Ok(false)` deliberately escalates to exhaustive discovery.
fn capture_partial_members<R: JsonlFamilyRuntime>(
    adapter: &dyn JsonlFamilyAdapter<Runtime = R>,
    root: &Path,
    resident: &Mutex<FamilyResident<JsonlRuntimeError<R>>>,
    sink: &mut SourceBackedGenerationSink<'_, R::Lifecycle>,
    members: &BTreeSet<PathBuf>,
) -> SourceBackedRouteResult<bool> {
    if members.is_empty()
        || sink.reconciliation_demand()
            != ctx_history_capture_runtime::SourceBackedReconciliationDemand::Incremental
    {
        return Ok(false);
    }
    let opened =
        open_partial_members(adapter, root, members).map_err(|error| route_scan(adapter, error))?;
    let Some(mut leaves) = opened else {
        return Ok(false);
    };
    if leaves.len() != members.len() {
        return Ok(false);
    }
    adapter
        .order_leaf_scans(&mut leaves)
        .map_err(|error| route_scan(adapter, error))?;
    let owned_elsewhere = leaves
        .iter()
        .any(|leaf| sink.source_owned_by_other_route(leaf.source()));
    if owned_elsewhere {
        return Ok(false);
    }

    let mut bases = Vec::with_capacity(leaves.len());
    let mut owned_sources = HashMap::with_capacity(leaves.len());
    for leaf in &leaves {
        let digest = leaf.source().exact_descriptor_digest();
        if owned_sources
            .insert(digest, leaf.source().clone())
            .is_some()
        {
            return Ok(false);
        }
        let Some(base) = sink.base_route_source(leaf.source()).cloned() else {
            return Ok(false);
        };
        match adapter.base_source_path(&base) {
            Ok(path) if path == leaf.source_path() => {}
            Ok(_) => {
                return Ok(false);
            }
            Err(_) => {
                return Ok(false);
            }
        }
        let Ok(checkpoint) = decode_checkpoint(adapter, leaf, &base) else {
            return Ok(false);
        };
        let retained = checkpoint.physical.source_observation();
        let current = leaf.observation();
        let unchanged = retained == current;
        let append_candidate =
            current.length() > retained.length() && retained.same_stable_file(current);
        if !unchanged && !append_candidate {
            return Ok(false);
        }
        bases.push(base);
    }
    reset_terminal(resident)?;
    let bases_by_descriptor = bases_by_descriptor(&bases)?;
    let base_event_lookup = sink.base_event_lookup();
    let terminal_sources = scan_leaves(
        adapter,
        &leaves,
        &bases_by_descriptor,
        base_event_lookup,
        sink,
        true,
    );
    let finish_leaf_scans = adapter
        .finish_leaf_scans()
        .map_err(|error| route_scan(adapter, error));
    let terminal_sources = terminal_sources?;
    finish_leaf_scans?;
    if terminal_sources.len() != leaves.len() {
        return Err(route_internal(
            "partial JSONL scan did not produce one terminal proof per selected member",
        ));
    }
    sink.retain_unstaged_base_route_sources()
        .map_err(route_internal)?;
    let mut resident = resident
        .lock()
        .map_err(|_| route_internal("JSONL resident catalog lock was poisoned"))?;
    // Unselected route members remain owned by the immutable carried base even
    // though this attempt intentionally has no complete-inventory proof.
    resident.ownership_initialized = false;
    resident.owned_sources = owned_sources;
    resident.terminal_sources = terminal_sources;
    resident.absent_sources.clear();
    resident.opening_membership = None;
    resident.certified_inventory = None;
    resident.opening_inventory = None;
    Ok(true)
}

fn open_partial_members<R: JsonlFamilyRuntime>(
    adapter: &dyn JsonlFamilyAdapter<Runtime = R>,
    _root: &Path,
    members: &BTreeSet<PathBuf>,
) -> JsonlPartialLeavesResult<R> {
    let Some(root_paths) = adapter.partial_member_roots(_root) else {
        return Ok(None);
    };
    if root_paths.is_empty() {
        return Ok(None);
    }
    let mut authorities = Vec::with_capacity(root_paths.len());
    for root_path in root_paths {
        authorities.push(Arc::new(ProviderSourceRoot::open(&lexical_absolute::<
            JsonlRuntimeError<R>,
        >(&root_path)?)?));
    }

    let mut normalized_members = BTreeSet::new();
    let mut leaves = Vec::with_capacity(members.len());
    for requested in members {
        let source_path = lexical_absolute::<JsonlRuntimeError<R>>(requested)?;
        if !normalized_members.insert(source_path.clone())
            || source_path.as_os_str().as_encoded_bytes().len()
                > PROVIDER_JSONL_INVENTORY_MAX_PATH_BYTES
        {
            return Ok(None);
        }
        let matches = authorities
            .iter()
            .filter_map(|authority| {
                source_path
                    .strip_prefix(authority.named_path())
                    .ok()
                    .filter(|relative| !relative.as_os_str().is_empty())
                    .map(|relative| (Arc::clone(authority), relative.to_path_buf()))
            })
            .collect::<Vec<_>>();
        let [(authority, authority_path)] = matches.as_slice() else {
            return Ok(None);
        };
        if authority_path.components().count()
            > PROVIDER_JSONL_INVENTORY_MAX_DEPTH.saturating_add(1)
        {
            return Ok(None);
        }
        let opened = match authority.open_file(authority_path) {
            Ok(opened) => opened,
            Err(_) => return Ok(None),
        };
        let observation = observe_opened_file_allow_append(&source_path, &opened)?;
        let member = JsonlFamilyOpenedMember {
            source_path,
            authority_path: authority_path.clone(),
            authority: Arc::clone(authority),
            opened: &opened,
            observation,
        };
        let Some(leaf) = adapter.bind_partial_member(&member)? else {
            return Ok(None);
        };
        if leaf.source_path() != member.source_path()
            || leaf.authority_path != member.authority_path
            || leaf.authority.named_path() != member.authority.named_path()
            || leaf.observation() != member.observation()
        {
            return Ok(None);
        }
        leaves.push(leaf);
    }
    for authority in authorities {
        authority.revalidate_same_object()?;
    }
    Ok(Some(leaves))
}

fn lexical_absolute<E: JsonlFamilyError>(path: &Path) -> JsonlResult<PathBuf, E> {
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
                if !normalized.pop() {
                    return Err(E::invalid_payload(
                        "partial JSONL member escapes its filesystem root".to_owned(),
                    ));
                }
            }
            _ => normalized.push(component.as_os_str()),
        }
    }
    Ok(normalized)
}

fn bases_by_descriptor(
    bases: &[CertifiedSource],
) -> SourceBackedRouteResult<HashMap<[u8; 32], &CertifiedSource>> {
    let mut by_descriptor = HashMap::with_capacity(bases.len());
    for base in bases {
        let source = base.observation().source();
        let digest = source.exact_descriptor_digest();
        if let Some(previous) = by_descriptor.insert(digest, base) {
            if !previous.observation().source().exact_descriptor_eq(source) {
                return Err(route_invalid(
                    "JSONL base source descriptor digest collision",
                ));
            }
            return Err(route_invalid("duplicate JSONL base source descriptor"));
        }
    }
    Ok(by_descriptor)
}
