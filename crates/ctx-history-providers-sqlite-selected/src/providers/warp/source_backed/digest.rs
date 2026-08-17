use super::*;

pub(super) fn missing_tree_fingerprint(source: &SourceKey) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(WARP_MISSING_TREE_DOMAIN);
    digest.update(source.exact_descriptor_digest());
    digest.finalize().into()
}

pub(super) fn checked_add(left: u64, right: u64) -> WarpSourceBackedResultV0<u64> {
    left.checked_add(right)
        .ok_or(WarpSourceBackedErrorV0::CountOverflow)
}

pub(super) fn hash_bytes(digest: &mut Sha256, value: &[u8]) -> WarpSourceBackedResultV0<()> {
    digest.update(
        u64::try_from(value.len())
            .map_err(|_| WarpSourceBackedErrorV0::CountOverflow)?
            .to_be_bytes(),
    );
    digest.update(value);
    Ok(())
}

pub(super) fn hash_text(digest: &mut Sha256, value: &str) {
    digest.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_be_bytes());
    digest.update(value.as_bytes());
}

pub(super) fn hash_optional_text(digest: &mut Sha256, value: Option<&str>) {
    match value {
        Some(value) => {
            digest.update([1]);
            hash_text(digest, value);
        }
        None => digest.update([0]),
    }
}

pub(super) fn parse_hex_digest(value: &str) -> WarpSourceBackedResultV0<[u8; 32]> {
    digest_bytes(value)
}

pub(super) fn digest_bytes(value: &str) -> WarpSourceBackedResultV0<[u8; 32]> {
    if value.len() != 64
        || value
            .bytes()
            .any(|byte| !byte.is_ascii_hexdigit() || byte.is_ascii_uppercase())
    {
        return Err(WarpSourceBackedErrorV0::InvalidDigest);
    }
    let mut digest = [0_u8; 32];
    for (index, slot) in digest.iter_mut().enumerate() {
        let offset = index * 2;
        *slot = u8::from_str_radix(&value[offset..offset + 2], 16)
            .map_err(|_| WarpSourceBackedErrorV0::InvalidDigest)?;
    }
    Ok(digest)
}
