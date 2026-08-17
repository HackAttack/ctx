use super::*;

#[test]
fn portable_entry_contract_rejects_links_directories_and_special_files() {
    assert!(require_regular(EntryKind::Regular).is_ok());
    for kind in [
        EntryKind::Directory,
        EntryKind::LinkOrReparse,
        EntryKind::Special,
    ] {
        assert!(matches!(
            require_regular(kind),
            Err(IndexError::CurrentRepublishSourceTopology(_))
        ));
    }
}
