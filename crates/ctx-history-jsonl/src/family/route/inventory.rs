use super::*;

#[derive(Debug)]
pub struct JsonlFamilyInventory<E: JsonlFamilyError> {
    pub(super) root_missing: bool,
    pub(super) observation: SourceInventoryObservation,
    pub(super) authorities: Vec<Arc<ProviderSourceRoot<E>>>,
    pub(super) leaves: Vec<JsonlFamilyLeaf<E>>,
    pub(super) rejected_leaves: Vec<JsonlFamilyRejectedLeaf>,
    pub(super) exact_dependencies: Vec<JsonlFamilyTerminalProof<E>>,
}

impl<E: JsonlFamilyError> Clone for JsonlFamilyInventory<E> {
    fn clone(&self) -> Self {
        Self {
            root_missing: self.root_missing,
            observation: self.observation.clone(),
            authorities: self.authorities.clone(),
            leaves: self.leaves.clone(),
            rejected_leaves: self.rejected_leaves.clone(),
            exact_dependencies: self.exact_dependencies.clone(),
        }
    }
}

impl<E: JsonlFamilyError> JsonlFamilyInventory<E> {
    pub fn present(
        provider: CaptureProvider,
        root: &Path,
        authority: Arc<ProviderSourceRoot<E>>,
        leaves: Vec<JsonlFamilyLeaf<E>>,
    ) -> JsonlResult<Self, E> {
        Self::present_with_rejected(provider, root, authority, leaves, Vec::new())
    }

    pub fn present_with_rejected(
        provider: CaptureProvider,
        root: &Path,
        authority: Arc<ProviderSourceRoot<E>>,
        leaves: Vec<JsonlFamilyLeaf<E>>,
        rejected_leaves: Vec<JsonlFamilyRejectedLeaf>,
    ) -> JsonlResult<Self, E> {
        Self::present_multi_with_rejected(provider, root, vec![authority], leaves, rejected_leaves)
    }

    pub fn present_multi(
        provider: CaptureProvider,
        root: &Path,
        authorities: Vec<Arc<ProviderSourceRoot<E>>>,
        leaves: Vec<JsonlFamilyLeaf<E>>,
    ) -> JsonlResult<Self, E> {
        Self::present_multi_with_rejected(provider, root, authorities, leaves, Vec::new())
    }

    pub fn present_multi_with_rejected(
        provider: CaptureProvider,
        root: &Path,
        mut authorities: Vec<Arc<ProviderSourceRoot<E>>>,
        mut leaves: Vec<JsonlFamilyLeaf<E>>,
        mut rejected_leaves: Vec<JsonlFamilyRejectedLeaf>,
    ) -> JsonlResult<Self, E> {
        if authorities.is_empty() {
            return Err(E::invalid_payload(
                "present JSONL inventory has no retained root authority".to_owned(),
            ));
        }
        authorities.sort_by(|left, right| left.named_path().cmp(right.named_path()));
        for pair in authorities.windows(2) {
            if pair[0].named_path() == pair[1].named_path() {
                return Err(E::invalid_payload(format!(
                    "present JSONL inventory has duplicate root authority {}",
                    pair[0].named_path().display()
                )));
            }
        }
        for leaf in &leaves {
            let retained = authorities.iter().any(|authority| {
                authority.named_path() == leaf.authority.named_path()
                    && authority.authority_fingerprint() == leaf.authority.authority_fingerprint()
            });
            if !retained {
                return Err(E::invalid_payload(format!(
                    "JSONL leaf {} is outside the retained root authorities",
                    leaf.source_path.display()
                )));
            }
        }
        leaves.sort_by(|left, right| left.source_path.cmp(&right.source_path));
        rejected_leaves.sort_by(|left, right| left.source_path.cmp(&right.source_path));
        let observation = inventory_observation(
            provider,
            root,
            false,
            &authorities,
            &leaves,
            &rejected_leaves,
        )?;
        Ok(Self {
            root_missing: false,
            observation,
            authorities,
            leaves,
            rejected_leaves,
            exact_dependencies: Vec::new(),
        })
    }

    pub fn missing(provider: CaptureProvider, root: &Path) -> JsonlResult<Self, E> {
        Ok(Self {
            root_missing: true,
            observation: inventory_observation::<E>(provider, root, true, &[], &[], &[])?,
            authorities: Vec::new(),
            leaves: Vec::new(),
            rejected_leaves: Vec::new(),
            exact_dependencies: Vec::new(),
        })
    }

    pub fn with_exact_dependencies(
        mut self,
        exact_dependencies: Vec<JsonlFamilyTerminalProof<E>>,
    ) -> Self {
        self.exact_dependencies = exact_dependencies;
        self
    }

    pub fn root_missing(&self) -> bool {
        self.root_missing
    }

    pub fn leaves(&self) -> &[JsonlFamilyLeaf<E>] {
        &self.leaves
    }

    pub fn rejected_leaves(&self) -> &[JsonlFamilyRejectedLeaf] {
        &self.rejected_leaves
    }

    #[cfg(test)]
    pub(super) fn certify_against(
        &self,
        closing: &Self,
    ) -> JsonlResult<CertifiedSourceInventory, E> {
        self.certify_selected_against(
            closing,
            closing
                .leaves
                .iter()
                .map(|leaf| leaf.source.clone())
                .collect(),
        )
    }

    pub(super) fn certify_selected_against(
        &self,
        closing: &Self,
        sources: Vec<SourceKey>,
    ) -> JsonlResult<CertifiedSourceInventory, E> {
        if self.root_missing != closing.root_missing {
            return Err(E::invalid_payload(
                "JSONL root availability changed during capture".to_owned(),
            ));
        }
        CertifiedSourceInventory::certify(
            self.observation.clone(),
            closing.observation.clone(),
            FAMILY_DISCOVERY_REVISION,
            sources,
        )
        .map_err(contract_error)
    }

    pub(super) fn revalidate_root(&self) -> JsonlResult<(), E> {
        if self.root_missing {
            return Ok(());
        }
        if self.authorities.is_empty() {
            return Err(E::invalid_payload(
                "JSONL inventory has no retained root authority".to_owned(),
            ));
        }
        for authority in &self.authorities {
            authority.revalidate()?;
        }
        Ok(())
    }

    pub(super) fn revalidate_root_same_object(&self) -> JsonlResult<(), E> {
        if self.root_missing {
            return Ok(());
        }
        if self.authorities.is_empty() {
            return Err(E::invalid_payload(
                "JSONL inventory has no retained root authority".to_owned(),
            ));
        }
        for authority in &self.authorities {
            authority.revalidate_same_object()?;
        }
        Ok(())
    }

    pub(super) fn revalidate_terminal_root(
        &self,
        root: &Path,
        mode: JsonlFamilyInventoryMode,
    ) -> JsonlResult<(), E> {
        if self.root_missing {
            return match open_provider_source_path::<E>(root) {
                Err(error) if error.is_not_found() => Ok(()),
                Ok(_) => Err(E::source_changed()),
                Err(error) => Err(error),
            };
        }
        match mode {
            JsonlFamilyInventoryMode::Exact => self.revalidate_root(),
            JsonlFamilyInventoryMode::FrozenOpeningAllowAdditions => {
                self.revalidate_root_same_object()
            }
        }
    }
}
