use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, Weak},
};

use base64::{engine::general_purpose::STANDARD_NO_PAD, Engine as _};
use ctx_history_core::{CORE_RECORD_VERSION, IDENTITY_VERSION};
use ctx_history_index_generation::{
    load_manifest_bytes, manifest_path, sha256_hex, write_manifest_bytes,
};
use serde::{Deserialize, Serialize};
use tantivy::{IndexMeta, Searcher};

use crate::{
    expected_source_generation_policy_hash, is_generation_id, validate_core_contract_fingerprint,
    CommitPayload, GenerationManifest, IndexError, Result, SourceCoreRecordAggregate,
    COMMIT_PAYLOAD_VERSION, GENERATION_MANIFEST_VERSION, LEXICAL_ANALYZER_VERSION,
    LEXICAL_SCHEMA_VERSION, MAX_PUBLICATION_METADATA_BYTES,
};

use ctx_history_core::CertifiedSource;

const MAX_PUBLICATION_METADATA_ENCODED_BYTES: usize =
    MAX_PUBLICATION_METADATA_BYTES.div_ceil(3) * 4;
const MAX_COMMIT_PAYLOAD_BYTES: usize = MAX_PUBLICATION_METADATA_ENCODED_BYTES + 256;
const MANIFEST_DELTA_STORAGE: &str = "ctx-manifest-delta-v1";
const MAX_MANIFEST_DELTA_CHANGES: usize = 64;
const MAX_MANIFEST_DELTA_BYTES: usize = 1024 * 1024;

type ManifestCacheKey = (PathBuf, String);
static MANIFEST_CACHE: OnceLock<Mutex<BTreeMap<ManifestCacheKey, Weak<GenerationManifest>>>> =
    OnceLock::new();

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredManifestDeltaV1 {
    storage_format: String,
    base_generation_id: String,
    indexed_documents: u64,
    certified_source_bytes: u64,
    source_count: usize,
    changes: Vec<StoredManifestSourceChangeV1>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredManifestSourceChangeV1 {
    source_identity: [u8; 32],
    source: CertifiedSource,
    aggregate: SourceCoreRecordAggregate,
}

pub struct PreparedManifest {
    generation_id: String,
    bytes: Vec<u8>,
}

impl PreparedManifest {
    pub fn generation_id(&self) -> &str {
        &self.generation_id
    }
}

#[derive(Debug)]
pub struct LoadedPublication {
    generation_id: String,
    manifest: Arc<GenerationManifest>,
    metadata: Option<Arc<[u8]>>,
}

impl LoadedPublication {
    pub fn generation_id(&self) -> &str {
        &self.generation_id
    }

    pub fn manifest(&self) -> &GenerationManifest {
        &self.manifest
    }

    pub fn metadata(&self) -> Option<&Arc<[u8]>> {
        self.metadata.as_ref()
    }

    #[doc(hidden)]
    pub fn into_parts(self) -> (String, Arc<GenerationManifest>, Option<Arc<[u8]>>) {
        (self.generation_id, self.manifest, self.metadata)
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct BorrowedCommitPayload<'a> {
    version: u32,
    #[serde(borrow)]
    generation_id: &'a str,
    #[serde(borrow)]
    publication_metadata: Option<&'a str>,
}

#[derive(Debug)]
struct DecodedCommitPayload {
    generation_id: String,
    publication_metadata: Option<Vec<u8>>,
}

pub fn load_publication_for_metas(root: &Path, metas: &IndexMeta) -> Result<LoadedPublication> {
    let payload = decode_commit_payload(
        metas
            .payload
            .as_deref()
            .ok_or(IndexError::MissingCommitPayload)?,
    )?;
    let manifest = load_materialized_manifest(root, &payload.generation_id, 0)?;
    Ok(LoadedPublication {
        generation_id: payload.generation_id,
        manifest,
        metadata: payload
            .publication_metadata
            .map(|metadata| Arc::from(metadata.into_boxed_slice())),
    })
}

fn load_materialized_manifest(
    root: &Path,
    generation_id: &str,
    depth: usize,
) -> Result<Arc<GenerationManifest>> {
    if depth > 128 {
        return Err(IndexError::NonCanonicalManifest);
    }
    let key = (root.to_path_buf(), generation_id.to_owned());
    if let Some(manifest) = MANIFEST_CACHE
        .get_or_init(|| Mutex::new(BTreeMap::new()))
        .lock()
        .map_err(|_| IndexError::NonCanonicalManifest)?
        .get(&key)
        .and_then(Weak::upgrade)
    {
        return Ok(manifest);
    }
    let bytes = load_manifest_bytes(root, generation_id)?;
    let manifest = if bytes.starts_with(br#"{"storage_format":"ctx-manifest-delta-v1","#) {
        let delta: StoredManifestDeltaV1 = serde_json::from_slice(&bytes)?;
        if serde_json::to_vec(&delta)? != bytes
            || delta.storage_format != MANIFEST_DELTA_STORAGE
            || !is_generation_id(&delta.base_generation_id)
            || delta.changes.is_empty()
            || delta.changes.len() > MAX_MANIFEST_DELTA_CHANGES
        {
            return Err(IndexError::NonCanonicalManifest);
        }
        let base = load_materialized_manifest(root, &delta.base_generation_id, depth + 1)?;
        materialize_delta(base.as_ref(), delta)?
    } else {
        let manifest: GenerationManifest = serde_json::from_slice(&bytes)?;
        if serde_json::to_vec(&manifest)? != bytes {
            return Err(IndexError::NonCanonicalManifest);
        }
        validate_manifest_contract(&manifest)?;
        manifest
    };
    let manifest = Arc::new(manifest);
    let mut cache = MANIFEST_CACHE
        .get_or_init(|| Mutex::new(BTreeMap::new()))
        .lock()
        .map_err(|_| IndexError::NonCanonicalManifest)?;
    cache.retain(|_, manifest| manifest.strong_count() != 0);
    cache.insert(key, Arc::downgrade(&manifest));
    Ok(manifest)
}

fn materialize_delta(
    base: &GenerationManifest,
    delta: StoredManifestDeltaV1,
) -> Result<GenerationManifest> {
    if delta.source_count != base.sources.len() {
        return Err(IndexError::NonCanonicalManifest);
    }
    let mut replacements = Vec::with_capacity(delta.changes.len());
    let mut previous = None;
    for change in delta.changes {
        if previous.is_some_and(|digest| digest >= change.source_identity) {
            return Err(IndexError::NonCanonicalManifest);
        }
        previous = Some(change.source_identity);
        let source_index = base
            .sources
            .binary_search_by_key(&change.source_identity, |source| {
                source.observation().source().identity().digest()
            })
            .map_err(|_| IndexError::NonCanonicalManifest)?;
        if change.source.observation().source().identity().digest() != change.source_identity {
            return Err(IndexError::NonCanonicalManifest);
        }
        let source_identity_hex = hex_digest(change.source_identity);
        let aggregate_index = base
            .core_record_aggregates
            .binary_search_by(|aggregate| {
                aggregate.source_identity_digest().cmp(&source_identity_hex)
            })
            .map_err(|_| IndexError::NonCanonicalManifest)?;
        if base.core_record_aggregates[aggregate_index].source_identity_digest()
            != change.aggregate.source_identity_digest()
        {
            return Err(IndexError::NonCanonicalManifest);
        }
        if source_index != aggregate_index {
            return Err(IndexError::NonCanonicalManifest);
        }
        replacements.push((change.source, change.aggregate));
    }
    let materialized = base.apply_validated_source_replacements(replacements)?;
    if materialized.indexed_documents != delta.indexed_documents
        || materialized.certified_source_bytes != delta.certified_source_bytes
        || materialized.sources.len() != delta.source_count
    {
        return Err(IndexError::NonCanonicalManifest);
    }
    Ok(materialized)
}

fn validate_manifest_contract(manifest: &GenerationManifest) -> Result<()> {
    if manifest.manifest_version != GENERATION_MANIFEST_VERSION {
        return Err(IndexError::UnsupportedManifest(manifest.manifest_version));
    }
    if manifest.identity_version != IDENTITY_VERSION
        || manifest.lexical_schema_version != LEXICAL_SCHEMA_VERSION
        || manifest.lexical_analyzer_version != LEXICAL_ANALYZER_VERSION
        || manifest.core_record_version != CORE_RECORD_VERSION
    {
        return Err(IndexError::GenerationContractMismatch {
            identity: manifest.identity_version,
            schema: manifest.lexical_schema_version,
            analyzer: manifest.lexical_analyzer_version,
            core_record: manifest.core_record_version,
        });
    }
    validate_core_contract_fingerprint(&manifest.core_record_contract_fingerprint)?;
    let expected_policy_hash = expected_source_generation_policy_hash()?;
    if manifest.policy_schema_hash != expected_policy_hash {
        return Err(IndexError::GenerationPolicyMismatch {
            expected: expected_policy_hash,
            actual: manifest.policy_schema_hash.clone(),
        });
    }
    manifest.validate_contract()
}

fn hex_digest(digest: [u8; 32]) -> String {
    digest
        .into_iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub fn canonical_commit_payload(
    generation_id: &str,
    publication_metadata: Option<&[u8]>,
) -> Result<String> {
    if !is_generation_id(generation_id) {
        return Err(IndexError::InvalidGenerationId);
    }
    let publication_metadata = publication_metadata
        .map(|metadata| {
            if metadata.len() > MAX_PUBLICATION_METADATA_BYTES {
                return Err(IndexError::PublicationMetadataTooLarge {
                    actual: metadata.len(),
                    maximum: MAX_PUBLICATION_METADATA_BYTES,
                });
            }
            Ok(STANDARD_NO_PAD.encode(metadata))
        })
        .transpose()?;
    Ok(serde_json::to_string(&CommitPayload {
        version: COMMIT_PAYLOAD_VERSION,
        generation_id: generation_id.to_owned(),
        publication_metadata,
    })?)
}

fn decode_commit_payload(encoded: &str) -> Result<DecodedCommitPayload> {
    if encoded.len() > MAX_COMMIT_PAYLOAD_BYTES {
        return Err(IndexError::CommitPayloadTooLarge {
            actual: encoded.len(),
            maximum: MAX_COMMIT_PAYLOAD_BYTES,
        });
    }
    let payload: BorrowedCommitPayload<'_> = serde_json::from_str(encoded)?;
    if payload.version != COMMIT_PAYLOAD_VERSION {
        return Err(IndexError::UnsupportedCommitPayload(payload.version));
    }
    if !is_generation_id(payload.generation_id) {
        return Err(IndexError::InvalidGenerationId);
    }
    let publication_metadata_decoded_len = payload
        .publication_metadata
        .map(|metadata| {
            let decoded_len = unpadded_base64_decoded_len(metadata.len())?;
            if decoded_len > MAX_PUBLICATION_METADATA_BYTES {
                return Err(IndexError::PublicationMetadataTooLarge {
                    actual: decoded_len,
                    maximum: MAX_PUBLICATION_METADATA_BYTES,
                });
            }
            Ok(decoded_len)
        })
        .transpose()?;
    if serde_json::to_string(&payload)? != encoded {
        return Err(IndexError::NonCanonicalCommitPayload);
    }
    let publication_metadata = payload
        .publication_metadata
        .zip(publication_metadata_decoded_len)
        .map(|(metadata, decoded_len)| {
            let decoded = STANDARD_NO_PAD
                .decode(metadata)
                .map_err(|_| IndexError::InvalidPublicationMetadataEncoding)?;
            if decoded.len() != decoded_len {
                return Err(IndexError::InvalidPublicationMetadataEncoding);
            }
            Ok(decoded)
        })
        .transpose()?;
    Ok(DecodedCommitPayload {
        generation_id: payload.generation_id.to_owned(),
        publication_metadata,
    })
}

fn unpadded_base64_decoded_len(encoded_len: usize) -> Result<usize> {
    let trailing = match encoded_len % 4 {
        0 => 0,
        2 => 1,
        3 => 2,
        _ => return Err(IndexError::InvalidPublicationMetadataEncoding),
    };
    encoded_len
        .checked_div(4)
        .and_then(|groups| groups.checked_mul(3))
        .and_then(|prefix| prefix.checked_add(trailing))
        .ok_or(IndexError::CountOverflow)
}

pub fn reconcile_commit_error(
    index: &tantivy::Index,
    expected_generation_id: &str,
    previous_generation_id: Option<&str>,
    commit_error: tantivy::TantivyError,
) -> Result<u64> {
    let metas = index.load_metas().map_err(|reconcile_error| {
        IndexError::CommittedGenerationNeedsRecovery {
            generation_id: expected_generation_id.to_owned(),
            stage: "commit reconciliation",
            detail: format!("{commit_error}; reopening meta.json failed: {reconcile_error}"),
        }
    })?;
    let visible_generation = payload_generation_id(&metas).map_err(|payload_error| {
        IndexError::CommittedGenerationNeedsRecovery {
            generation_id: expected_generation_id.to_owned(),
            stage: "commit reconciliation",
            detail: format!("{commit_error}; candidate payload is invalid: {payload_error}"),
        }
    })?;
    if visible_generation.as_deref() == Some(expected_generation_id) {
        return Ok(metas.opstamp);
    }
    if visible_generation.as_deref() == previous_generation_id
        || (previous_generation_id.is_none()
            && visible_generation.is_none()
            && metas.segments.is_empty())
    {
        return Err(IndexError::Tantivy(commit_error));
    }
    Err(IndexError::CommittedGenerationNeedsRecovery {
        generation_id: expected_generation_id.to_owned(),
        stage: "candidate commit reconciliation",
        detail: format!(
            "{commit_error}; expected old generation {:?} or candidate generation, found {:?}",
            previous_generation_id, visible_generation
        ),
    })
}

pub fn payload_generation_id(metas: &IndexMeta) -> Result<Option<String>> {
    let Some(payload) = metas.payload.as_deref() else {
        return Ok(None);
    };
    Ok(Some(decode_commit_payload(payload)?.generation_id))
}

pub fn write_manifest(
    root: &Path,
    generation_id: &str,
    manifest: &GenerationManifest,
) -> Result<()> {
    if manifest_path(root, generation_id).is_file() {
        let retained = load_materialized_manifest(root, generation_id, 0)?;
        if serde_json::to_vec(retained.as_ref())? == serde_json::to_vec(manifest)? {
            return Ok(());
        }
        return Err(IndexError::NonCanonicalManifest);
    }
    let bytes = serde_json::to_vec(manifest)?;
    Ok(write_manifest_bytes(root, generation_id, &bytes)?)
}

pub fn prepare_successor_manifest(
    manifest: &GenerationManifest,
    base: Option<(&str, &GenerationManifest)>,
) -> Result<PreparedManifest> {
    let full = || -> Result<PreparedManifest> {
        let bytes = serde_json::to_vec(manifest)?;
        Ok(PreparedManifest {
            generation_id: sha256_hex(&bytes),
            bytes,
        })
    };
    let Some((base_generation_id, base)) = base else {
        return full();
    };
    if !is_generation_id(base_generation_id)
        || base.sources.len() != manifest.sources.len()
        || base.core_record_aggregates.len() != manifest.core_record_aggregates.len()
        || base.source_routes().len() != manifest.source_routes().len()
        || base
            .source_routes()
            .iter()
            .zip(manifest.source_routes())
            .any(|(base, current)| !base.exact_snapshot_eq(current))
        || base.manifest_version != manifest.manifest_version
        || base.identity_version != manifest.identity_version
        || base.core_record_version != manifest.core_record_version
        || base.core_record_contract_fingerprint != manifest.core_record_contract_fingerprint
        || base.lexical_schema_version != manifest.lexical_schema_version
        || base.lexical_analyzer_version != manifest.lexical_analyzer_version
        || base.policy_schema_hash != manifest.policy_schema_hash
    {
        return full();
    }
    let mut changes = Vec::new();
    for ((base_source, source), (base_aggregate, aggregate)) in
        base.sources.iter().zip(&manifest.sources).zip(
            base.core_record_aggregates
                .iter()
                .zip(&manifest.core_record_aggregates),
        )
    {
        let source_identity = source.observation().source().identity().digest();
        if base_source.observation().source().identity().digest() != source_identity
            || base_aggregate.source_identity_digest() != aggregate.source_identity_digest()
            || aggregate.source_identity_digest() != hex_digest(source_identity)
        {
            return full();
        }
        let source_changed =
            !base_source.shares_immutable_parts_with(source) && base_source != source;
        if source_changed || base_aggregate != aggregate {
            changes.push(StoredManifestSourceChangeV1 {
                source_identity,
                source: source.clone(),
                aggregate: aggregate.clone(),
            });
        }
    }
    if changes.is_empty() || changes.len() > MAX_MANIFEST_DELTA_CHANGES {
        return full();
    }
    let delta = StoredManifestDeltaV1 {
        storage_format: MANIFEST_DELTA_STORAGE.to_owned(),
        base_generation_id: base_generation_id.to_owned(),
        indexed_documents: manifest.indexed_documents,
        certified_source_bytes: manifest.certified_source_bytes,
        source_count: manifest.sources.len(),
        changes,
    };
    let bytes = serde_json::to_vec(&delta)?;
    if bytes.len() > MAX_MANIFEST_DELTA_BYTES {
        return full();
    }
    Ok(PreparedManifest {
        generation_id: sha256_hex(&bytes),
        bytes,
    })
}

pub fn write_prepared_manifest(root: &Path, manifest: &PreparedManifest) -> Result<()> {
    Ok(write_manifest_bytes(
        root,
        &manifest.generation_id,
        &manifest.bytes,
    )?)
}

pub fn meta_generation(metas: &IndexMeta) -> BTreeMap<String, Option<u64>> {
    metas
        .segments
        .iter()
        .map(|segment| (segment.id().uuid_string(), segment.delete_opstamp()))
        .collect()
}

pub fn searcher_generation(searcher: &Searcher) -> BTreeMap<String, Option<u64>> {
    searcher
        .segment_readers()
        .iter()
        .map(|segment| (segment.segment_id().uuid_string(), segment.delete_opstamp()))
        .collect()
}
