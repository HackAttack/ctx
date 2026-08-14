use super::*;

pub(super) fn source_route_content_fingerprints(
    snapshot: Option<&impl ImmutableCaptureSnapshot>,
) -> HashMap<SourceRouteIdentity, [u8; 32]> {
    let Some(snapshot) = snapshot else {
        return HashMap::new();
    };
    let aggregates = snapshot
        .sources()
        .iter()
        .zip(snapshot.source_aggregates())
        .map(|(source, aggregate)| (source.observation().source().identity().digest(), aggregate))
        .collect::<HashMap<_, _>>();
    snapshot
        .source_routes()
        .map(|route| {
            (
                route.route_identity().clone(),
                source_route_content_fingerprint(route.sources(), &aggregates),
            )
        })
        .collect()
}

pub(super) fn empty_source_route_content_fingerprint() -> [u8; 32] {
    source_route_content_fingerprint(&[], &HashMap::new())
}

fn source_route_content_fingerprint(
    sources: &[SourceKey],
    aggregates: &HashMap<[u8; 32], CaptureSourceAggregateRef<'_>>,
) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"ctx.source-route-content-v2\0");
    digest.update((sources.len() as u64).to_be_bytes());
    for source in sources {
        digest.update(source.identity().digest());
        match aggregates.get(&source.identity().digest()) {
            Some(aggregate) => {
                digest.update([1]);
                digest.update(aggregate.indexed_documents().to_be_bytes());
                digest.update(aggregate.core_record_accumulator().as_bytes());
            }
            None => digest.update([0]),
        }
    }
    digest.finalize().into()
}
