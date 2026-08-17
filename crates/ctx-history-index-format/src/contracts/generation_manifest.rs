use super::*;

impl GenerationManifest {
    #[cfg(any(test, feature = "test-support"))]
    pub fn from_sources(sources: Vec<CertifiedSource>) -> Result<Self> {
        let aggregates = test_aggregates(&sources)?;
        let source_routes = implicit_source_routes(&sources)?;
        Self::from_parts_with_record_aggregates(sources, aggregates, source_routes)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn from_parts(
        sources: Vec<CertifiedSource>,
        source_routes: Vec<SourceRouteSnapshot>,
    ) -> Result<Self> {
        let aggregates = test_aggregates(&sources)?;
        Self::from_parts_with_record_aggregates(sources, aggregates, source_routes)
    }

    pub fn from_parts_with_record_aggregates(
        mut sources: Vec<CertifiedSource>,
        mut core_record_aggregates: Vec<SourceCoreRecordAggregate>,
        mut source_routes: Vec<SourceRouteSnapshot>,
    ) -> Result<Self> {
        sources.sort_by(|left, right| {
            source_sort_key(left.observation().source())
                .cmp(&source_sort_key(right.observation().source()))
        });
        if sources.windows(2).any(|pair| {
            source_sort_key(pair[0].observation().source())
                >= source_sort_key(pair[1].observation().source())
        }) {
            return Err(IndexError::NonCanonicalManifestSources);
        }
        source_routes.sort_by(|left, right| left.route_identity.cmp(&right.route_identity));
        core_record_aggregates.sort_by(|left, right| {
            left.source_identity_digest
                .cmp(&right.source_identity_digest)
        });
        let mut indexed_documents = 0_u64;
        let mut certified_source_bytes = 0_u64;
        for (source, _aggregate) in sources.iter().zip(&core_record_aggregates) {
            indexed_documents = indexed_documents
                .checked_add(source.counts().indexed_documents)
                .ok_or(IndexError::CountOverflow)?;
            certified_source_bytes = certified_source_bytes
                .checked_add(source.counts().certified_bytes)
                .ok_or(IndexError::CountOverflow)?;
        }
        let manifest = Self {
            manifest_version: GENERATION_MANIFEST_VERSION,
            identity_version: IDENTITY_VERSION,
            core_record_version: CORE_RECORD_VERSION,
            core_record_contract_fingerprint: current_core_record_contract_fingerprint(),
            lexical_schema_version: LEXICAL_SCHEMA_VERSION,
            lexical_analyzer_version: LEXICAL_ANALYZER_VERSION,
            policy_schema_hash: current_source_generation_policy_hash()?,
            indexed_documents,
            certified_source_bytes,
            sources,
            core_record_aggregates,
            source_routes,
        };
        manifest.validate_contract()?;
        Ok(manifest)
    }

    pub fn generation_id(&self) -> Result<String> {
        Ok(sha256_hex(&serde_json::to_vec(self)?))
    }

    /// Compares the complete logical snapshot independently of its persisted
    /// descriptor encoding. A compact manifest descriptor may materialize to
    /// the same snapshot while having a different descriptor generation ID.
    pub fn exact_snapshot_eq(&self, other: &Self) -> bool {
        self.manifest_version == other.manifest_version
            && self.identity_version == other.identity_version
            && self.core_record_version == other.core_record_version
            && self.core_record_contract_fingerprint == other.core_record_contract_fingerprint
            && self.lexical_schema_version == other.lexical_schema_version
            && self.lexical_analyzer_version == other.lexical_analyzer_version
            && self.policy_schema_hash == other.policy_schema_hash
            && self.indexed_documents == other.indexed_documents
            && self.certified_source_bytes == other.certified_source_bytes
            && self.sources == other.sources
            && self.core_record_aggregates == other.core_record_aggregates
            && self.source_routes.len() == other.source_routes.len()
            && self
                .source_routes
                .iter()
                .zip(&other.source_routes)
                .all(|(left, right)| left.exact_snapshot_eq(right))
    }

    pub(crate) fn apply_validated_source_replacements(
        &self,
        mut replacements: Vec<(CertifiedSource, SourceCoreRecordAggregate)>,
    ) -> Result<Self> {
        replacements.sort_by_key(|(source, _)| source_sort_key(source.observation().source()));
        if replacements.is_empty()
            || replacements.windows(2).any(|pair| {
                source_sort_key(pair[0].0.observation().source())
                    >= source_sort_key(pair[1].0.observation().source())
            })
        {
            return Err(IndexError::NonCanonicalManifestSources);
        }
        let mut sources = self.sources.clone();
        let mut aggregates = self.core_record_aggregates.clone();
        let mut indexed_documents = self.indexed_documents;
        let mut certified_source_bytes = self.certified_source_bytes;
        for (source, aggregate) in replacements {
            source.validate_contract()?;
            aggregate.validate_contract()?;
            let source_identity = source_sort_key(source.observation().source());
            let source_index = sources
                .binary_search_by_key(&source_identity, |candidate| {
                    source_sort_key(candidate.observation().source())
                })
                .map_err(|_| IndexError::NonCanonicalManifestSources)?;
            if !sources[source_index]
                .observation()
                .source()
                .exact_descriptor_eq(source.observation().source())
            {
                return Err(IndexError::NonCanonicalManifestSources);
            }
            let source_id = source_token(source.observation().source());
            if aggregate.source_identity_digest != source_id
                || aggregate.indexed_documents != source.counts().indexed_documents
            {
                return Err(IndexError::CoreRecordAggregateMismatch(source_id));
            }
            indexed_documents = indexed_documents
                .checked_sub(sources[source_index].counts().indexed_documents)
                .and_then(|count| count.checked_add(source.counts().indexed_documents))
                .ok_or(IndexError::CountOverflow)?;
            certified_source_bytes = certified_source_bytes
                .checked_sub(sources[source_index].counts().certified_bytes)
                .and_then(|count| count.checked_add(source.counts().certified_bytes))
                .ok_or(IndexError::CountOverflow)?;
            sources[source_index] = source;
            aggregates[source_index] = aggregate;
        }
        Ok(Self {
            manifest_version: self.manifest_version,
            identity_version: self.identity_version,
            core_record_version: self.core_record_version,
            core_record_contract_fingerprint: self.core_record_contract_fingerprint.clone(),
            lexical_schema_version: self.lexical_schema_version,
            lexical_analyzer_version: self.lexical_analyzer_version,
            policy_schema_hash: self.policy_schema_hash.clone(),
            indexed_documents,
            certified_source_bytes,
            sources,
            core_record_aggregates: aggregates,
            source_routes: self.source_routes.clone(),
        })
    }

    pub fn source_routes(&self) -> &[SourceRouteSnapshot] {
        &self.source_routes
    }

    pub fn source_route(
        &self,
        route_identity: &SourceRouteIdentity,
    ) -> Option<&SourceRouteSnapshot> {
        self.source_routes
            .binary_search_by(|candidate| candidate.route_identity().cmp(route_identity))
            .ok()
            .and_then(|index| self.source_routes.get(index))
    }

    pub(crate) fn validate_contract(&self) -> Result<()> {
        if self.sources.windows(2).any(|pair| {
            source_sort_key(pair[0].observation().source())
                >= source_sort_key(pair[1].observation().source())
        }) {
            return Err(IndexError::NonCanonicalManifestSources);
        }
        if self
            .source_routes
            .windows(2)
            .any(|pair| pair[0].route_identity() >= pair[1].route_identity())
        {
            return Err(IndexError::NonCanonicalSourceRoutes);
        }
        if self
            .core_record_aggregates
            .windows(2)
            .any(|pair| pair[0].source_identity_digest >= pair[1].source_identity_digest)
        {
            return Err(IndexError::CoreRecordAggregateMismatch(
                "non-canonical aggregate ordering".to_owned(),
            ));
        }
        let mut owned_sources = Vec::new();
        for route in &self.source_routes {
            route.validate_contract()?;
            for route_source in route.sources() {
                let retained = self.sources.binary_search_by(|candidate| {
                    source_sort_key(candidate.observation().source())
                        .cmp(&source_sort_key(route_source))
                });
                let is_exactly_retained = retained
                    .ok()
                    .and_then(|index| self.sources.get(index))
                    .is_some_and(|source| {
                        source
                            .observation()
                            .source()
                            .exact_descriptor_eq(route_source)
                    });
                if !is_exactly_retained {
                    return Err(IndexError::SourceRouteMemberNotRetained {
                        route_id: route.route_identity().as_str().to_owned(),
                        source_id: route_source.identity().to_string(),
                    });
                }
                owned_sources.push(source_sort_key(route_source));
            }
        }
        owned_sources.sort();
        if let Some(duplicate) = owned_sources.windows(2).find(|pair| pair[0] == pair[1]) {
            return Err(IndexError::SourceOwnedByMultipleRoutes(hex(&duplicate[0])));
        }
        for source in &self.sources {
            let key = source_sort_key(source.observation().source());
            if owned_sources.binary_search(&key).is_err() {
                return Err(IndexError::SourceNotOwnedByRoute(
                    source.observation().source().identity().to_string(),
                ));
            }
        }
        let mut expected_documents = 0_u64;
        let mut expected_bytes = 0_u64;
        for (source_index, source) in self.sources.iter().enumerate() {
            source.validate_contract()?;
            let source_id = crate::source_token(source.observation().source());
            let aggregate = self
                .core_record_aggregates
                .get(source_index)
                .ok_or_else(|| IndexError::CoreRecordAggregateMismatch(source_id.clone()))?;
            aggregate.validate_contract()?;
            if aggregate.source_identity_digest != source_id {
                return Err(IndexError::CoreRecordAggregateMismatch(source_id));
            }
            if aggregate.indexed_documents != source.counts().indexed_documents {
                return Err(IndexError::CoreRecordAggregateCountMismatch {
                    source_id: aggregate.source_identity_digest.clone(),
                    manifest: source.counts().indexed_documents,
                    index: aggregate.indexed_documents,
                });
            }
            expected_documents = expected_documents
                .checked_add(source.counts().indexed_documents)
                .ok_or(IndexError::CountOverflow)?;
            expected_bytes = expected_bytes
                .checked_add(source.counts().certified_bytes)
                .ok_or(IndexError::CountOverflow)?;
        }
        if self.core_record_aggregates.len() != self.sources.len() {
            return Err(IndexError::CoreRecordAggregateMismatch(
                "manifest aggregate cardinality".to_owned(),
            ));
        }
        if self.indexed_documents != expected_documents
            || self.certified_source_bytes != expected_bytes
        {
            return Err(IndexError::InvalidManifestTotals {
                documents: self.indexed_documents,
                expected_documents,
                bytes: self.certified_source_bytes,
                expected_bytes,
            });
        }
        Ok(())
    }
}

