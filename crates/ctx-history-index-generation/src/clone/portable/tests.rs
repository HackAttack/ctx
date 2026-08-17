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

#[test]
fn portable_authenticated_growth_probe_never_writes_the_extra_byte() {
    let mut source = io::Cursor::new(b"abcde".to_vec());
    let mut destination = Vec::new();
    assert!(matches!(
        copy_with_digest(&mut source, &mut destination, 4, 4),
        Err(IndexError::CurrentRepublishSourceTopology(
            "source file grew while cloning"
        ))
    ));
    assert_eq!(destination, b"abcd");

    let mut source = io::Cursor::new(b"abcde".to_vec());
    let mut destination = Vec::new();
    assert!(matches!(
        copy_with_digest(&mut source, &mut destination, 5, 4),
        Err(IndexError::CurrentRepublishByteLimit {
            actual: 5,
            maximum: 4
        })
    ));
    assert!(destination.is_empty());
}
