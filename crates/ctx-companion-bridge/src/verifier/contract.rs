use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AuthorityRegistry {
    pub(super) contract: String,
    pub(super) schema_version: u32,
    pub(super) channels: Vec<AuthorityChannel>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AuthorityChannel {
    pub(super) id: String,
    pub(super) key_id: String,
    pub(super) signature_algorithm: String,
    pub(super) public_key_der_sha256: String,
    pub(super) public_key_pem: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Envelope {
    pub(super) schema_version: u32,
    pub(super) manifest_base64: String,
    pub(super) signature_base64: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Manifest {
    pub(super) contract: String,
    pub(super) schema_version: u32,
    pub(super) channel: String,
    pub(super) release_authority_key_id: String,
    pub(super) release_name: String,
    pub(super) target: TargetDocument,
    pub(super) install_geometry: InstallGeometry,
    pub(super) target_matrix_sha256: String,
    pub(super) rollback_generation: u64,
    pub(super) snapshot: SnapshotDocument,
    pub(super) compatibility: CompatibilityDocument,
    pub(super) components: ComponentsDocument,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TargetDocument {
    pub(super) id: String,
    pub(super) os: String,
    pub(super) arch: String,
    pub(super) core_rust_target: String,
    pub(super) companion_rust_target: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct InstallGeometry {
    pub(super) install_root: String,
    pub(super) managed_bin_dir: String,
    pub(super) core_slot: String,
    pub(super) companion_slot: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SnapshotDocument {
    pub(super) contract: String,
    pub(super) fingerprint: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CompatibilityDocument {
    pub(super) invocation_fingerprint: String,
    pub(super) core_capability_fingerprint: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ComponentsDocument {
    pub(super) core: ComponentDocument,
    pub(super) companion: ComponentDocument,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ComponentDocument {
    pub(super) artifact_name: String,
    pub(super) object_key: String,
    pub(super) sha256: String,
    pub(super) size_bytes: u64,
    pub(super) install_slot: String,
    pub(super) build_identity: BuildIdentityDocument,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BuildIdentityDocument {
    pub(super) component: String,
    pub(super) rust_target: String,
    pub(super) source_revision: String,
    pub(super) build_fingerprint: String,
}
