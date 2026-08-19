use super::*;

pub(super) struct FamilyResident<E: JsonlFamilyError> {
    pub(super) ownership_initialized: bool,
    pub(super) owned_sources: HashMap<[u8; 32], SourceKey>,
    pub(super) quarantined_sources: HashMap<[u8; 32], SourceKey>,
    pub(super) terminal_sources: HashMap<[u8; 32], TerminalSourceEvidence<E>>,
    pub(super) absent_sources: Vec<JsonlFamilyAbsentMember<E>>,
    pub(super) opening_membership: Option<JsonlFamilyMembershipObservation<E>>,
    pub(super) certified_inventory: Option<CertifiedSourceInventory>,
    pub(super) opening_inventory: Option<JsonlFamilyInventory<E>>,
}

impl<E: JsonlFamilyError> Default for FamilyResident<E> {
    fn default() -> Self {
        Self {
            ownership_initialized: false,
            owned_sources: HashMap::new(),
            quarantined_sources: HashMap::new(),
            terminal_sources: HashMap::new(),
            absent_sources: Vec::new(),
            opening_membership: None,
            certified_inventory: None,
            opening_inventory: None,
        }
    }
}
