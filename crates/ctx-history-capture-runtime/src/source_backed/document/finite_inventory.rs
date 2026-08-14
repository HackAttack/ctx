use ctx_history_core::SourceKey;

/// One complete finite inventory and its provider-owned leaves.
pub struct FiniteInventoryCatalog<L, P> {
    pub authority_fingerprint: [u8; 32],
    pub leaves: Vec<FiniteInventoryCatalogLeaf<L, P>>,
}

/// One catalog slot. Physical locators remain generic and capture-owned.
pub struct FiniteInventoryCatalogLeaf<L, P> {
    pub source: SourceKey,
    pub physical_locator: P,
    pub provider_leaf: L,
}

/// Terminal authority retained from discovery through publication.
#[derive(Debug)]
pub struct FiniteInventoryTreeAuthority<F> {
    authority_fingerprint: [u8; 32],
    catalog_leaves: Vec<F>,
}

impl<F> FiniteInventoryTreeAuthority<F> {
    pub fn new(authority_fingerprint: [u8; 32], catalog_leaves: Vec<F>) -> Self {
        Self {
            authority_fingerprint,
            catalog_leaves,
        }
    }

    pub fn validates_slot(&self, index: usize, current: &F) -> bool
    where
        F: PartialEq,
    {
        self.catalog_leaves.get(index) == Some(current)
    }

    pub fn validates_complete(&self, authority_fingerprint: [u8; 32], current: &[F]) -> bool
    where
        F: PartialEq,
    {
        self.authority_fingerprint == authority_fingerprint && self.catalog_leaves == current
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finite_inventory_authority_preserves_ordered_slots_and_complete_revision() {
        let authority = FiniteInventoryTreeAuthority::new([7; 32], vec![[1; 32], [2; 32]]);
        assert!(authority.validates_slot(0, &[1; 32]));
        assert!(authority.validates_slot(1, &[2; 32]));
        assert!(!authority.validates_slot(0, &[2; 32]));
        assert!(!authority.validates_slot(2, &[3; 32]));
        assert!(authority.validates_complete([7; 32], &[[1; 32], [2; 32]]));
        assert!(!authority.validates_complete([8; 32], &[[1; 32], [2; 32]]));
        assert!(!authority.validates_complete([7; 32], &[[2; 32], [1; 32]]));
    }
}
