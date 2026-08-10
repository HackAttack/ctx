use super::*;

pub(super) fn retained_catalog_witness(
    retained_generation: Option<&VerifiedIndex>,
) -> Result<(
    Option<ExplicitSourceCatalogAuthority>,
    Vec<ExplicitSourceCatalogRouteBinding>,
)> {
    let Some(generation) = retained_generation else {
        return Ok((None, Vec::new()));
    };
    if generation.publication_metadata().is_none() {
        return Ok((None, Vec::new()));
    }
    let metadata = SourceBackedPublicationMetadata::decode(generation)
        .context("decode retained explicit catalog generation witness")?;
    let receipt = published_refresh_receipt_for_index(&metadata.response_value(), generation)
        .context("validate retained explicit catalog generation witness")?;
    Ok((
        receipt.published_explicit_source_catalog,
        receipt.catalog_route_bindings,
    ))
}
