use std::fmt;

use ctx_history_capture_model::ProviderSource;
use ctx_history_capture_runtime::{
    replacement_document_tree_driver, DocumentInventoryAuthority, SourceBackedRouteError,
    SourceBackedRouteErrorKind, SourceBackedRouteSelection, SourceBackedSelectorAuthority,
};
use sha2::{Digest, Sha256};

use crate::{ProviderReplacementDocumentTree, ProviderRouteDriver, ProviderRuntimeBinding};

type ExactDueInterpreter = fn(&[u8], [u8; 32], i64) -> Option<bool>;
type RetirementIdentityInterpreter = fn(&[u8]) -> Option<[u8; 32]>;

/// Provider-owned interpretation of opaque persisted route-control bytes.
///
/// Function pointers are fixed at registration and called only by refresh or
/// publication coordination; provider hot paths keep their existing bytes and
/// perform no extra allocation or I/O.
#[derive(Clone, Copy)]
pub struct ProviderRouteControlExpectation {
    interpreter_id: &'static str,
    owner_descriptor: [u8; 32],
    exact_due: ExactDueInterpreter,
    retirement_identity: Option<RetirementIdentityInterpreter>,
}

impl ProviderRouteControlExpectation {
    pub const fn new(
        interpreter_id: &'static str,
        owner_descriptor: [u8; 32],
        exact_due: ExactDueInterpreter,
        retirement_identity: Option<RetirementIdentityInterpreter>,
    ) -> Self {
        Self {
            interpreter_id,
            owner_descriptor,
            exact_due,
            retirement_identity,
        }
    }

    pub fn exact_due(&self, control: &[u8], now_ms: i64) -> Option<bool> {
        (self.exact_due)(control, self.owner_descriptor, now_ms)
    }

    pub fn retirement_identity(&self, control: &[u8]) -> Option<[u8; 32]> {
        self.retirement_identity
            .and_then(|interpret| interpret(control))
    }

    pub const fn owner_descriptor(&self) -> [u8; 32] {
        self.owner_descriptor
    }

    pub const fn interpreter_id(&self) -> &'static str {
        self.interpreter_id
    }

    pub const fn supports_retirement_identity(&self) -> bool {
        self.retirement_identity.is_some()
    }
}

impl fmt::Debug for ProviderRouteControlExpectation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderRouteControlExpectation")
            .field("interpreter_id", &self.interpreter_id)
            .field("owner_descriptor", &"<sha256>")
            .field(
                "has_retirement_identity",
                &self.retirement_identity.is_some(),
            )
            .finish()
    }
}

impl PartialEq for ProviderRouteControlExpectation {
    fn eq(&self, other: &Self) -> bool {
        self.interpreter_id == other.interpreter_id
            && self.owner_descriptor == other.owner_descriptor
            && self.supports_retirement_identity() == other.supports_retirement_identity()
    }
}

impl Eq for ProviderRouteControlExpectation {}

pub struct ProviderRouteRegistration<B: ProviderRuntimeBinding> {
    pub source: ProviderSource,
    pub selection: SourceBackedRouteSelection,
    pub selector_authority: SourceBackedSelectorAuthority,
    pub driver: ProviderRouteDriver<B>,
}

/// Downward registration port implemented by the capture composition root.
pub trait ProviderRouteRegistrar<B: ProviderRuntimeBinding> {
    type Error;

    fn register_provider_route(
        &mut self,
        registration: ProviderRouteRegistration<B>,
    ) -> std::result::Result<(), Self::Error>;
}

pub fn register_replacement_document_tree_route<B, R, A>(
    registrar: &mut R,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    selector_authority: SourceBackedSelectorAuthority,
    adapter: A,
) -> std::result::Result<(), R::Error>
where
    B: ProviderRuntimeBinding,
    R: ProviderRouteRegistrar<B>,
    A: ProviderReplacementDocumentTree<B>,
{
    let driver = provider_replacement_document_tree_driver::<B, A>(&source, adapter);
    registrar.register_provider_route(ProviderRouteRegistration {
        source,
        selection,
        selector_authority,
        driver,
    })
}

pub fn provider_replacement_document_tree_driver<B, A>(
    source: &ProviderSource,
    adapter: A,
) -> ProviderRouteDriver<B>
where
    B: ProviderRuntimeBinding,
    A: ProviderReplacementDocumentTree<B>,
{
    replacement_document_tree_driver(document_inventory_authority(source), adapter)
}

fn document_inventory_authority(route: &ProviderSource) -> DocumentInventoryAuthority {
    let path = route.path.as_os_str().as_encoded_bytes();
    let mut digest = Sha256::new();
    digest.update(b"ctx.document-tree-route-authority-v1\0");
    digest.update((route.provider.as_str().len() as u64).to_be_bytes());
    digest.update(route.provider.as_str().as_bytes());
    digest.update((route.source_format.len() as u64).to_be_bytes());
    digest.update(route.source_format.as_bytes());
    digest.update((path.len() as u64).to_be_bytes());
    digest.update(path);
    DocumentInventoryAuthority::new(route.provider.as_str().to_owned(), digest.finalize().into())
}

pub fn invalid_route_error(error: impl fmt::Display) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::InvalidSource, error.to_string())
}

pub fn combine_primary_and_cleanup_route_errors(
    primary: SourceBackedRouteError,
    cleanup: SourceBackedRouteError,
) -> SourceBackedRouteError {
    let kind = if route_error_severity(primary.kind) >= route_error_severity(cleanup.kind) {
        primary.kind
    } else {
        cleanup.kind
    };
    SourceBackedRouteError::new(
        kind,
        format!(
            "{}; explicit SQLite snapshot cleanup also failed: {}",
            primary.detail, cleanup.detail
        ),
    )
}

const fn route_error_severity(kind: SourceBackedRouteErrorKind) -> u8 {
    match kind {
        SourceBackedRouteErrorKind::Internal => 6,
        SourceBackedRouteErrorKind::ResourceUnavailable => 5,
        SourceBackedRouteErrorKind::SourceChanged => 4,
        SourceBackedRouteErrorKind::InvalidSource => 3,
        SourceBackedRouteErrorKind::Unsupported => 2,
        SourceBackedRouteErrorKind::Unavailable => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn due(control: &[u8], owner: [u8; 32], now_ms: i64) -> Option<bool> {
        Some(control == owner && now_ms >= 10)
    }

    fn retirement(control: &[u8]) -> Option<[u8; 32]> {
        control.try_into().ok()
    }

    #[test]
    fn route_control_is_static_owner_bound_and_allocation_free() {
        let owner = [7; 32];
        let expectation =
            ProviderRouteControlExpectation::new("test-control-v1", owner, due, Some(retirement));
        assert_eq!(expectation.exact_due(&owner, 9), Some(false));
        assert_eq!(expectation.exact_due(&owner, 10), Some(true));
        assert_eq!(expectation.retirement_identity(&owner), Some(owner));
        assert_eq!(expectation.owner_descriptor(), owner);
        assert_ne!(
            expectation,
            ProviderRouteControlExpectation::new("test-control-v1", owner, due, None)
        );
    }

    #[test]
    fn cleanup_composition_preserves_the_stronger_failure_class() {
        let primary = SourceBackedRouteError::new(SourceBackedRouteErrorKind::InvalidSource, "bad");
        let cleanup =
            SourceBackedRouteError::new(SourceBackedRouteErrorKind::ResourceUnavailable, "full");
        let combined = combine_primary_and_cleanup_route_errors(primary, cleanup);
        assert_eq!(
            combined.kind,
            SourceBackedRouteErrorKind::ResourceUnavailable
        );
        assert_eq!(
            combined.detail,
            "bad; explicit SQLite snapshot cleanup also failed: full"
        );
    }
}
