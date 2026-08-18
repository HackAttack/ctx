use super::*;

#[cfg(any(test, feature = "test-support"))]
struct BeforeTerminalPhysicalRevalidationHook {
    root: PathBuf,
    hook: Box<dyn FnOnce() + Send>,
}

#[cfg(any(test, feature = "test-support"))]
static BEFORE_TERMINAL_PHYSICAL_REVALIDATION_HOOKS: Mutex<
    Vec<BeforeTerminalPhysicalRevalidationHook>,
> = Mutex::new(Vec::new());

#[cfg(any(test, feature = "test-support"))]
pub fn set_before_jsonl_terminal_physical_revalidation_hook(
    root: PathBuf,
    hook: impl FnOnce() + Send + 'static,
) {
    let mut hooks = BEFORE_TERMINAL_PHYSICAL_REVALIDATION_HOOKS
        .lock()
        .expect("JSONL terminal physical-revalidation hook lock was poisoned");
    assert!(
        hooks.iter().all(|pending| pending.root != root),
        "JSONL terminal physical-revalidation hook is already installed for {root:?}"
    );
    hooks.push(BeforeTerminalPhysicalRevalidationHook {
        root,
        hook: Box::new(hook),
    });
}

#[cfg(any(test, feature = "test-support"))]
fn run_before_jsonl_terminal_physical_revalidation_hook(root: &Path) {
    let hook = {
        let mut hooks = BEFORE_TERMINAL_PHYSICAL_REVALIDATION_HOOKS
            .lock()
            .expect("JSONL terminal physical-revalidation hook lock was poisoned");
        hooks
            .iter()
            .position(|pending| pending.root == root)
            .map(|index| hooks.remove(index).hook)
    };
    if let Some(hook) = hook {
        hook();
    }
}

pub(super) fn reset_terminal<E: JsonlFamilyError>(
    resident: &Mutex<FamilyResident<E>>,
) -> SourceBackedRouteResult<()> {
    let mut resident = resident
        .lock()
        .map_err(|_| route_internal("JSONL resident catalog lock was poisoned"))?;
    resident.terminal_sources.clear();
    resident.absent_sources.clear();
    resident.opening_membership = None;
    resident.certified_inventory = None;
    resident.opening_inventory = None;
    Ok(())
}

pub(super) fn revalidate_target<E: JsonlFamilyError>(
    resident: &Mutex<FamilyResident<E>>,
    target: SourceBackedRevalidationTarget<'_>,
) -> bool {
    let Ok(resident) = resident.lock() else {
        return false;
    };
    match target {
        SourceBackedRevalidationTarget::Source(expected) => {
            let Some(evidence) = resident
                .terminal_sources
                .get(&expected.observation().source().exact_descriptor_digest())
            else {
                return false;
            };
            evidence.certificate == *expected
        }
        SourceBackedRevalidationTarget::Deletion(deletion) => resident
            .certified_inventory
            .as_ref()
            .is_some_and(|inventory| {
                deletion.verifies(inventory)
                    && !resident
                        .terminal_sources
                        .contains_key(&deletion.source().exact_descriptor_digest())
            }),
    }
}

pub(super) fn revalidate_complete_inventory<R: JsonlFamilyRuntime>(
    adapter: &dyn JsonlFamilyAdapter<Runtime = R>,
    root: &Path,
    resident: &Mutex<FamilyResident<JsonlRuntimeError<R>>>,
    expected_inventory: &CertifiedSourceInventory,
) -> JsonlResult<bool, JsonlRuntimeError<R>> {
    let (
        owned_sources,
        expected_sources,
        absent_sources,
        opening_membership,
        certified_inventory,
        opening_inventory,
    ) = {
        let resident = resident.lock().map_err(|_| {
            JsonlRuntimeError::<R>::invalid_payload(
                "JSONL resident catalog lock was poisoned".to_owned(),
            )
        })?;
        (
            resident.owned_sources.clone(),
            resident.terminal_sources.clone(),
            resident.absent_sources.clone(),
            resident.opening_membership.clone(),
            resident.certified_inventory.clone(),
            resident.opening_inventory.clone(),
        )
    };
    if certified_inventory.as_ref() != Some(expected_inventory) {
        return Ok(false);
    }
    let Some(opening_inventory) = opening_inventory else {
        return Ok(false);
    };
    let Some(opening_membership) = opening_membership else {
        return Ok(false);
    };
    opening_inventory.revalidate_terminal_root(root, adapter.inventory_mode())?;

    let current_membership = adapter.observe_terminal_membership(root, &opening_inventory)?;
    if !opening_membership.admits(
        &current_membership,
        adapter.inventory_mode(),
        &expected_sources,
        &owned_sources,
    ) {
        return Ok(false);
    }

    // This is the single terminal filesystem witness for the route. It observes
    // only retained membership routes and their physical proofs; provider
    // discovery, identity probing, parsing, and content cataloging are admission
    // work and are never repeated here.
    #[cfg(any(test, feature = "test-support"))]
    run_before_jsonl_terminal_physical_revalidation_hook(root);
    for evidence in expected_sources.values() {
        evidence
            .terminal_proof
            .revalidate_for(evidence.observed_certificate())?;
    }
    for dependency in &opening_inventory.exact_dependencies {
        dependency.revalidate_dependency()?;
    }
    for absent in &absent_sources {
        if !absent.remains_absent()? {
            return Ok(false);
        }
    }
    opening_inventory.revalidate_terminal_root(root, adapter.inventory_mode())?;
    Ok(true)
}

pub(super) fn inventory_observation<E: JsonlFamilyError>(
    provider: CaptureProvider,
    root: &Path,
    missing: bool,
    authorities: &[Arc<ProviderSourceRoot<E>>],
    leaves: &[JsonlFamilyLeaf<E>],
    rejected_leaves: &[JsonlFamilyRejectedLeaf],
) -> JsonlResult<SourceInventoryObservation, E> {
    let mut digest = Sha256::new();
    digest.update(FAMILY_INVENTORY_DOMAIN);
    digest.update([u8::from(missing)]);
    digest.update((leaves.len() as u64).to_be_bytes());
    digest.update((rejected_leaves.len() as u64).to_be_bytes());
    match authorities {
        [] => {}
        [authority] => {
            // Preserve the v1 single-root digest exactly. Multi-root adapters
            // use an explicit extension below without perturbing existing
            // providers' generation identities.
            digest.update(authority.authority_fingerprint());
        }
        authorities => {
            digest.update(b"multi-root-authorities-v1\0");
            digest.update((authorities.len() as u64).to_be_bytes());
            for authority in authorities {
                let path = authority.named_path().as_os_str().as_encoded_bytes();
                digest.update((path.len() as u64).to_be_bytes());
                digest.update(path);
                digest.update(authority.authority_fingerprint());
            }
        }
    }
    for leaf in leaves {
        digest.update([0]);
        digest.update(leaf.source.exact_descriptor_digest());
        digest.update([u8::from(leaf.whole_record)]);
        digest.update(
            (leaf.authority_path.as_os_str().as_encoded_bytes().len() as u64).to_be_bytes(),
        );
        digest.update(leaf.authority_path.as_os_str().as_encoded_bytes());
        digest.update(binding_digest(leaf)?);
    }
    for leaf in rejected_leaves {
        digest.update([1]);
        digest.update(
            (leaf.authority_path.as_os_str().as_encoded_bytes().len() as u64).to_be_bytes(),
        );
        digest.update(leaf.authority_path.as_os_str().as_encoded_bytes());
        digest.update((leaf.source_path.as_os_str().as_encoded_bytes().len() as u64).to_be_bytes());
        digest.update(leaf.source_path.as_os_str().as_encoded_bytes());
        digest.update(serde_json::to_vec(&leaf.proof)?);
    }
    SourceInventoryObservation::new(
        provider.as_str(),
        FAMILY_INVENTORY_AUTHORITY,
        TypedKey::bytes(root.as_os_str().as_encoded_bytes().to_vec())
            .map_err(contract_error::<E>)?,
        FAMILY_INVENTORY_REVISION,
        digest.finalize().to_vec(),
    )
    .map_err(contract_error::<E>)
}

pub(super) fn binding_digest<E: JsonlFamilyError>(
    leaf: &JsonlFamilyLeaf<E>,
) -> JsonlResult<[u8; 32], E> {
    Ok(Sha256::digest(serde_json::to_vec(leaf.binding())?).into())
}
