use super::*;

pub(super) fn merge_manifest_sources(
    base: &[CertifiedSource],
    mut upserts: BTreeMap<[u8; 32], CertifiedSource>,
    deletions: &BTreeSet<[u8; 32]>,
) -> Vec<CertifiedSource> {
    let mut sources = Vec::with_capacity(base.len().saturating_add(upserts.len()));
    for certificate in base {
        #[cfg(any(test, feature = "test-support"))]
        BASE_MANIFEST_SOURCE_MATERIALIZATIONS
            .with(|visits| visits.set(visits.get().saturating_add(1)));
        let digest = source_sort_key(certificate.observation().source());
        if let Some(replacement) = upserts.remove(&digest) {
            sources.push(replacement);
        } else if !deletions.contains(&digest) {
            sources.push(certificate.clone());
        }
    }
    sources.extend(upserts.into_values());
    sources
}

pub(super) fn merge_partial_route_members(
    base: &[SourceKey],
    delta: &PartialSourceRouteDelta,
) -> Vec<SourceKey> {
    let mut upserts = delta.upserts.clone();
    let mut members = Vec::with_capacity(base.len().saturating_add(upserts.len()));
    for member in base {
        #[cfg(any(test, feature = "test-support"))]
        PARTIAL_BASE_ROUTE_MEMBER_MATERIALIZATIONS
            .with(|visits| visits.set(visits.get().saturating_add(1)));
        let digest = member.identity().digest();
        if let Some(replacement) = upserts.remove(&digest) {
            members.push(replacement);
        } else if !delta.deletions.contains(&digest) {
            members.push(member.clone());
        }
    }
    members.extend(upserts.into_values());
    members
}
