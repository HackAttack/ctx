use super::{document_contract_error, SourceBackedRouteResult};
use ctx_history_core::{CertifiedSourceInventory, SourceInventoryObservation, SourceKey, TypedKey};

const DOCUMENT_INVENTORY_AUTHORITY_NAMESPACE: &str = "ctx.document-tree";
const DOCUMENT_INVENTORY_REVISION_KIND: &str = "ctx-document-tree-fingerprint-v1";
const DOCUMENT_INVENTORY_DISCOVERY_REVISION: &str = "ctx-document-tree-discovery-v1";

#[derive(Clone)]
pub struct DocumentInventoryAuthority {
    provider: String,
    route_key: [u8; 32],
}

impl DocumentInventoryAuthority {
    /// Creates inventory authority from a capture-owned provider label and
    /// stable route key. Concrete route/path hashing stays above this runtime.
    pub fn new(provider: String, route_key: [u8; 32]) -> Self {
        Self {
            provider,
            route_key,
        }
    }

    pub fn certify(
        &self,
        tree_fingerprint: [u8; 32],
        sources: Vec<SourceKey>,
    ) -> SourceBackedRouteResult<CertifiedSourceInventory> {
        let observation = SourceInventoryObservation::new(
            self.provider.clone(),
            DOCUMENT_INVENTORY_AUTHORITY_NAMESPACE,
            TypedKey::bytes(self.route_key.to_vec()).map_err(document_contract_error)?,
            DOCUMENT_INVENTORY_REVISION_KIND,
            tree_fingerprint.to_vec(),
        )
        .map_err(document_contract_error)?;
        CertifiedSourceInventory::certify(
            observation.clone(),
            observation,
            DOCUMENT_INVENTORY_DISCOVERY_REVISION,
            sources,
        )
        .map_err(document_contract_error)
    }
}
