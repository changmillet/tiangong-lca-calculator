//! Shipped applicability metadata for an exact, immutable supported source release.
//!
//! The companion is additive: it never rewrites the original XML or the 1.2.4
//! numeric cache. Its compiled manifest and asset hashes are the trust boundary.
//! A source UUID, missing location, or user-provided receipt cannot authorize it.

use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    sync::OnceLock,
};

use flate2::read::GzDecoder;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    calculation_evidence::{
        LciaExchangeDirection, RELEASE_BUNDLE_MANIFEST_SHA256, RELEASE_FACTOR_MANIFEST_SHA256,
        canonical_json_sha256, sha256_bytes,
    },
    lcia_applicability::{FactorLocation, FactorRecord},
    lcia_source_xml::InspectedIlcdMethod,
    static_lcia_cache::{StaticLciaDirection, VerifiedStaticLciaBundle},
};

pub const SCHEMA_VERSION: &str = "lcia.supported-source-policy.v2";
pub const MANIFEST_SHA256: &str =
    "2ddb0fe715a2b6647246dd443f4fe417868683ca67bbea29c1a81ee7b955142d";
const MANIFEST: &[u8] = include_bytes!("lcia_source_policy_assets_v2/manifest.json");
const MAX_METHOD_BYTES: u64 = 16 * 1024 * 1024;
const NUMERIC_BINDING_HEADER: &[u8] = b"lcia.source-numeric-binding.v1\n";

/// The rule's scientific/source scope is retained; generic never means GLO.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyBasis {
    pub id: String,
    pub uri: String,
    pub locator: String,
    pub scope: String,
}

/// Generic record identity and geographic applicability are separate source claims.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum GenericGeographicAdmission {
    Global {
        source_geography_sha256: String,
    },
    ExactSourceTokens {
        source_geography_sha256: String,
        tokens: BTreeSet<String>,
    },
}

impl GenericGeographicAdmission {
    fn validate_source(&self, source_scope: &Value) -> anyhow::Result<()> {
        let geography = source_scope
            .pointer("/information/geography")
            .ok_or_else(|| anyhow::anyhow!("lcia_supported_policy_source_geography_missing"))?;
        let actual_hash = canonical_json_sha256(geography)?;
        let intervention = geography
            .get("interventionLocation")
            .and_then(Value::as_str);
        let valid = match self {
            Self::Global {
                source_geography_sha256,
            } => *source_geography_sha256 == actual_hash && intervention == Some("GLO"),
            Self::ExactSourceTokens {
                source_geography_sha256,
                tokens,
            } => {
                *source_geography_sha256 == actual_hash
                    && tokens.len() == 1
                    && intervention.is_some_and(|token| {
                        !token.trim().is_empty() && token.len() <= 256 && tokens.contains(token)
                    })
            }
        };
        anyhow::ensure!(valid, "lcia_supported_policy_geographic_binding_mismatch");
        Ok(())
    }

    fn admits(&self, location: &FactorLocation) -> bool {
        match self {
            Self::Global { .. } => {
                matches!(
                    location,
                    FactorLocation::Missing | FactorLocation::Blank | FactorLocation::Unknown
                ) || location.is_exact_token()
            }
            Self::ExactSourceTokens { tokens, .. } => {
                matches!(location, FactorLocation::Code(token) if tokens.contains(token))
            }
        }
    }
}

/// Persisted provenance only. The private validated policy grants selection authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourcePolicyReceipt {
    pub schema_version: String,
    pub manifest_sha256: String,
    pub companion_sha256: String,
    pub source_repository: String,
    pub source_commit: String,
    pub source_path: String,
    pub source_xml_sha256: String,
    pub source_metadata_sha256: String,
    pub source_scope_description: String,
    /// Historical receipts decode without granting executable geographic authority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generic_geographic_admission: Option<GenericGeographicAdmission>,
    pub numeric_binding_sha256: String,
    pub source_binding_kind: String,
    pub static_bundle_manifest_sha256: String,
    pub static_factor_manifest_sha256: String,
    pub factor_record_hash_kind: String,
    pub basis: Vec<PolicyBasis>,
    pub regional_default_unresolved_family_count: usize,
    pub reviewed_comment_count: usize,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyManifest {
    schema_version: String,
    policy_version: String,
    source_repository: String,
    source_commit: String,
    static_bundle_manifest_sha256: String,
    static_factor_manifest_sha256: String,
    record_hash_kind: String,
    numeric_binding: String,
    methods: Vec<MethodManifest>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MethodManifest {
    method_id: Uuid,
    method_version: String,
    artifact_locator_id: Uuid,
    source_path: String,
    source_xml_sha256: String,
    method_document_sha256: String,
    numeric_binding_sha256: String,
    metadata_sha256: String,
    generic_geographic_admission: GenericGeographicAdmission,
    basis: Vec<PolicyBasis>,
    regional_default_unresolved_family_count: usize,
    asset: String,
    asset_sha256: String,
    decompressed_sha256: String,
    decompressed_byte_size: u64,
    factor_count: usize,
    generic_record_count: usize,
    reviewed_comment_count: usize,
}

/// The array position is the original XML factor ordinal. No CF values are copied.
#[derive(Debug, Deserialize)]
struct FactorBinding(String, usize, String, usize, usize);

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MethodCompanion {
    method_id: Uuid,
    method_version: String,
    artifact_locator_id: Uuid,
    source_path: String,
    source_xml_sha256: String,
    method_document_sha256: String,
    numeric_binding_sha256: String,
    metadata_sha256: String,
    source_scope: Value,
    generic_geographic_admission: GenericGeographicAdmission,
    basis: Vec<PolicyBasis>,
    regional_default_unresolved_family_count: usize,
    locations: Vec<FactorLocation>,
    qualifiers: Vec<Value>,
    bindings: Vec<FactorBinding>,
    generic_records: BTreeSet<usize>,
    reviewed_comment_records: BTreeSet<usize>,
}

#[derive(Debug, Clone)]
pub(crate) struct SupportedSourcePolicy {
    pub receipt: SourcePolicyReceipt,
    generic_records: BTreeSet<usize>,
    reviewed_comment_records: BTreeSet<usize>,
    generic_geographic_admission: GenericGeographicAdmission,
}

impl SupportedSourcePolicy {
    pub(crate) fn is_generic(&self, record_index: usize) -> bool {
        self.generic_records.contains(&record_index)
    }

    pub(crate) fn admits_generic_location(&self, location: &FactorLocation) -> bool {
        self.generic_geographic_admission.admits(location)
    }

    pub(crate) fn review_qualifiers(&self, records: &mut [FactorRecord]) {
        for record in records {
            if self.reviewed_comment_records.contains(&record.record_index) {
                record
                    .unresolved_qualifier_fields
                    .retain(|field| field != "generalComment");
                record.unsupported_qualifiers = !record.unresolved_qualifier_fields.is_empty();
            }
        }
    }
}

fn manifest() -> anyhow::Result<&'static PolicyManifest> {
    static PARSED: OnceLock<Result<PolicyManifest, String>> = OnceLock::new();
    PARSED
        .get_or_init(|| {
            let load = || -> anyhow::Result<PolicyManifest> {
                anyhow::ensure!(
                    sha256_bytes(MANIFEST) == MANIFEST_SHA256,
                    "lcia_supported_policy_manifest_hash_mismatch"
                );
                let manifest: PolicyManifest = serde_json::from_slice(MANIFEST)?;
                anyhow::ensure!(
                    manifest.schema_version == SCHEMA_VERSION
                        && manifest.policy_version == crate::lcia_applicability::POLICY_VERSION
                        && manifest.static_bundle_manifest_sha256 == RELEASE_BUNDLE_MANIFEST_SHA256
                        && manifest.static_factor_manifest_sha256 == RELEASE_FACTOR_MANIFEST_SHA256
                        && manifest
                            .numeric_binding
                            .starts_with("lcia.source-numeric-binding.v1:")
                        && manifest.methods.len() == 25,
                    "lcia_supported_policy_manifest_binding_mismatch"
                );
                let methods = manifest
                    .methods
                    .iter()
                    .map(|method| method.method_id)
                    .collect::<BTreeSet<_>>();
                anyhow::ensure!(
                    methods.len() == manifest.methods.len(),
                    "lcia_supported_policy_duplicate_method"
                );
                Ok(manifest)
            };
            load().map_err(|error| error.to_string())
        })
        .as_ref()
        .map_err(|error| anyhow::anyhow!("{error}"))
}

fn method_entry(
    method_id: Uuid,
    method_version: &str,
    artifact_locator_id: Uuid,
) -> anyhow::Result<Option<&'static MethodManifest>> {
    Ok(manifest()?.methods.iter().find(|entry| {
        entry.method_id == method_id
            && entry.method_version == method_version
            && entry.artifact_locator_id == artifact_locator_id
    }))
}

fn decode_companion(entry: &MethodManifest) -> anyhow::Result<MethodCompanion> {
    let bytes = asset(&entry.asset)
        .ok_or_else(|| anyhow::anyhow!("lcia_supported_policy_asset_missing"))?;
    anyhow::ensure!(
        sha256_bytes(bytes) == entry.asset_sha256,
        "lcia_supported_policy_asset_hash_mismatch"
    );
    anyhow::ensure!(
        entry.decompressed_byte_size <= MAX_METHOD_BYTES,
        "lcia_supported_policy_asset_size_limit"
    );
    let mut decoded = Vec::new();
    GzDecoder::new(bytes)
        .take(MAX_METHOD_BYTES + 1)
        .read_to_end(&mut decoded)?;
    anyhow::ensure!(
        u64::try_from(decoded.len())? == entry.decompressed_byte_size
            && sha256_bytes(&decoded) == entry.decompressed_sha256,
        "lcia_supported_policy_decoded_hash_mismatch"
    );
    let companion: MethodCompanion = serde_json::from_slice(&decoded)?;
    anyhow::ensure!(
        companion.method_id == entry.method_id
            && companion.method_version == entry.method_version
            && companion.artifact_locator_id == entry.artifact_locator_id
            && companion.source_path == entry.source_path
            && companion.source_xml_sha256 == entry.source_xml_sha256
            && companion.method_document_sha256 == entry.method_document_sha256
            && companion.numeric_binding_sha256 == entry.numeric_binding_sha256
            && companion.metadata_sha256 == entry.metadata_sha256
            && companion.generic_geographic_admission == entry.generic_geographic_admission
            && canonical_json_sha256(&companion.source_scope)? == entry.metadata_sha256
            && companion.basis == entry.basis
            && companion.regional_default_unresolved_family_count
                == entry.regional_default_unresolved_family_count
            && companion.bindings.len() == entry.factor_count
            && companion.generic_records.len() == entry.generic_record_count
            && companion.reviewed_comment_records.len() == entry.reviewed_comment_count
            && companion
                .generic_records
                .iter()
                .chain(&companion.reviewed_comment_records)
                .all(|index| *index < entry.factor_count),
        "lcia_supported_policy_method_binding_mismatch"
    );
    companion
        .generic_geographic_admission
        .validate_source(&companion.source_scope)?;
    for binding in &companion.bindings {
        anyhow::ensure!(
            binding.3 < companion.locations.len() && binding.4 < companion.qualifiers.len(),
            "lcia_supported_policy_record_binding_invalid"
        );
    }
    Ok(companion)
}

fn bound_policy(
    entry: &MethodManifest,
    companion: &MethodCompanion,
    binding_kind: &str,
    factor_hash_kind: &str,
) -> anyhow::Result<SupportedSourcePolicy> {
    let manifest = manifest()?;
    Ok(SupportedSourcePolicy {
        receipt: SourcePolicyReceipt {
            schema_version: manifest.schema_version.clone(),
            manifest_sha256: MANIFEST_SHA256.to_owned(),
            companion_sha256: entry.asset_sha256.clone(),
            source_repository: manifest.source_repository.clone(),
            source_commit: manifest.source_commit.clone(),
            source_path: entry.source_path.clone(),
            source_xml_sha256: entry.source_xml_sha256.clone(),
            source_metadata_sha256: entry.metadata_sha256.clone(),
            generic_geographic_admission: Some(entry.generic_geographic_admission.clone()),
            source_scope_description: companion
                .source_scope
                .pointer("/information/geography/geographicalRepresentativenessDescription")
                .and_then(|value| value.as_str().or_else(|| value.get("#text")?.as_str()))
                .unwrap_or("Scope is retained in the exact source metadata")
                .to_owned(),
            numeric_binding_sha256: entry.numeric_binding_sha256.clone(),
            source_binding_kind: binding_kind.to_owned(),
            static_bundle_manifest_sha256: manifest.static_bundle_manifest_sha256.clone(),
            static_factor_manifest_sha256: manifest.static_factor_manifest_sha256.clone(),
            factor_record_hash_kind: factor_hash_kind.to_owned(),
            basis: entry.basis.clone(),
            regional_default_unresolved_family_count: entry
                .regional_default_unresolved_family_count,
            reviewed_comment_count: entry.reviewed_comment_count,
        },
        generic_records: companion.generic_records.clone(),
        reviewed_comment_records: companion.reviewed_comment_records.clone(),
        generic_geographic_admission: companion.generic_geographic_admission.clone(),
    })
}

pub(crate) fn bind_method_document(
    method_id: Uuid,
    method_version: &str,
    artifact_locator_id: Uuid,
    document_sha256: &str,
) -> anyhow::Result<Option<SupportedSourcePolicy>> {
    let Some(entry) = method_entry(method_id, method_version, artifact_locator_id)? else {
        return Ok(None);
    };
    if document_sha256 != entry.method_document_sha256 {
        return Ok(None);
    }
    let companion = decode_companion(entry)?;
    bound_policy(
        entry,
        &companion,
        "exact_native_method_adapter",
        "sha256-canonical-json-factor-adapter.v1",
    )
    .map(Some)
}

pub(crate) fn bind_native_source(
    source: &InspectedIlcdMethod,
    artifact_locator_id: Uuid,
) -> anyhow::Result<Option<SupportedSourcePolicy>> {
    let Some(entry) = method_entry(
        source.method_id,
        &source.method_version,
        artifact_locator_id,
    )?
    else {
        return Ok(None);
    };
    if source.raw_sha256 != entry.source_xml_sha256 {
        return Ok(None);
    }
    anyhow::ensure!(
        canonical_json_sha256(&source.method_document)? == entry.method_document_sha256,
        "lcia_supported_policy_native_adapter_mismatch"
    );
    let companion = decode_companion(entry)?;
    bound_policy(
        entry,
        &companion,
        "exact_raw_ilcd_xml",
        "sha256-canonical-native-xml-factor-subtree.v1",
    )
    .map(Some)
}

fn update_numeric_hash(
    hasher: &mut Sha256,
    flow_id: Uuid,
    flow_version: &str,
    direction: StaticLciaDirection,
    value: f64,
) {
    let direction = match direction {
        StaticLciaDirection::Input => "INPUT",
        StaticLciaDirection::Output => "OUTPUT",
    };
    hasher.update(format!(
        "{flow_id}|{flow_version}|{direction}|{:016x}\n",
        value.to_bits()
    ));
}

pub(crate) fn complete_static_method(
    bundle: &VerifiedStaticLciaBundle,
    method_id: Uuid,
    method_version: &str,
    artifact_locator_id: Uuid,
) -> anyhow::Result<Option<(Vec<FactorRecord>, SupportedSourcePolicy)>> {
    if bundle.source_evidence.bundle_manifest_sha256 != RELEASE_BUNDLE_MANIFEST_SHA256
        || bundle.source_evidence.factor_manifest_sha256 != RELEASE_FACTOR_MANIFEST_SHA256
    {
        return Ok(None);
    }
    let Some(entry) = method_entry(method_id, method_version, artifact_locator_id)? else {
        return Ok(None);
    };
    let companion = decode_companion(entry)?;
    let input = bundle
        .factors_by_method
        .get(&method_id)
        .ok_or_else(|| anyhow::anyhow!("lcia_supported_policy_static_method_missing"))?;
    anyhow::ensure!(
        input.len() == entry.factor_count,
        "lcia_supported_policy_static_factor_count_mismatch"
    );
    let indexed = input
        .iter()
        .map(|factor| {
            (
                (factor.source_index_key.as_str(), factor.source_factor_index),
                factor,
            )
        })
        .collect::<BTreeMap<_, _>>();
    anyhow::ensure!(
        indexed.len() == input.len(),
        "lcia_supported_policy_static_factor_duplicate"
    );
    let mut used = BTreeSet::new();
    let mut numeric = Sha256::new();
    numeric.update(NUMERIC_BINDING_HEADER);
    let mut records = Vec::with_capacity(input.len());
    for (record_index, binding) in companion.bindings.iter().enumerate() {
        let key = (binding.0.as_str(), binding.1);
        let factor = indexed
            .get(&key)
            .ok_or_else(|| anyhow::anyhow!("lcia_supported_policy_static_record_missing"))?;
        anyhow::ensure!(
            used.insert(key) && factor.value.is_finite(),
            "lcia_supported_policy_static_record_invalid"
        );
        update_numeric_hash(
            &mut numeric,
            factor.flow_id,
            &factor.flow_version,
            factor.direction,
            factor.value,
        );
        let qualifiers = companion.qualifiers[binding.4].clone();
        let comment = qualifiers.get("generalComment").and_then(Value::as_str);
        let unresolved_qualifier_fields = if comment.is_some_and(|value| !value.trim().is_empty())
            && !companion.reviewed_comment_records.contains(&record_index)
        {
            vec!["generalComment".to_owned()]
        } else {
            Vec::new()
        };
        records.push(FactorRecord {
            record_index,
            record_locator: format!("{}#factor/{record_index}", entry.source_path),
            record_sha256: binding.2.clone(),
            flow_id: factor.flow_id,
            flow_version: factor.flow_version.clone(),
            direction: match factor.direction {
                StaticLciaDirection::Input => LciaExchangeDirection::Input,
                StaticLciaDirection::Output => LciaExchangeDirection::Output,
            },
            location: companion.locations[binding.3].clone(),
            value: factor.value,
            unsupported_qualifiers: !unresolved_qualifier_fields.is_empty(),
            unresolved_qualifier_fields,
            qualifiers,
        });
    }
    anyhow::ensure!(
        used.len() == input.len()
            && hex::encode(numeric.finalize()) == entry.numeric_binding_sha256,
        "lcia_supported_policy_static_numeric_binding_mismatch"
    );
    let policy = bound_policy(
        entry,
        &companion,
        "verified_1.2.4_static_cache_and_shipped_companion",
        &manifest()?.record_hash_kind,
    )?;
    Ok(Some((records, policy)))
}

fn asset(path: &str) -> Option<&'static [u8]> {
    match path {
        "01500b74-7ffb-463e-9bd4-72f17c2263ff.json.gz" => Some(include_bytes!(
            "lcia_source_policy_assets_v2/01500b74-7ffb-463e-9bd4-72f17c2263ff.json.gz"
        )),
        "05316e7a-b254-4bea-9cf0-6bf33eb5c630.json.gz" => Some(include_bytes!(
            "lcia_source_policy_assets_v2/05316e7a-b254-4bea-9cf0-6bf33eb5c630.json.gz"
        )),
        "14af9ca7-aa1d-4832-b1d9-ab05a06dcb12.json.gz" => Some(include_bytes!(
            "lcia_source_policy_assets_v2/14af9ca7-aa1d-4832-b1d9-ab05a06dcb12.json.gz"
        )),
        "2299222a-bbd8-474f-9d4f-4dd1f18aea7c.json.gz" => Some(include_bytes!(
            "lcia_source_policy_assets_v2/2299222a-bbd8-474f-9d4f-4dd1f18aea7c.json.gz"
        )),
        "9ec743ea-6b00-400d-a53b-61547a3fc03c.json.gz" => Some(include_bytes!(
            "lcia_source_policy_assets_v2/9ec743ea-6b00-400d-a53b-61547a3fc03c.json.gz"
        )),
        "6209b35f-9447-40b5-b68c-a1099e3674a0.json.gz" => Some(include_bytes!(
            "lcia_source_policy_assets_v2/6209b35f-9447-40b5-b68c-a1099e3674a0.json.gz"
        )),
        "706261af-a357-4cc0-a50a-f3033fcbd556.json.gz" => Some(include_bytes!(
            "lcia_source_policy_assets_v2/706261af-a357-4cc0-a50a-f3033fcbd556.json.gz"
        )),
        "7cfdcfcf-b222-4b26-888a-a55f9fbf7ac8.json.gz" => Some(include_bytes!(
            "lcia_source_policy_assets_v2/7cfdcfcf-b222-4b26-888a-a55f9fbf7ac8.json.gz"
        )),
        "7fce5b3a-66b8-4ce1-91e8-a925aee1f186.json.gz" => Some(include_bytes!(
            "lcia_source_policy_assets_v2/7fce5b3a-66b8-4ce1-91e8-a925aee1f186.json.gz"
        )),
        "8c3141e9-1f15-43b5-bff2-182e49893a46.json.gz" => Some(include_bytes!(
            "lcia_source_policy_assets_v2/8c3141e9-1f15-43b5-bff2-182e49893a46.json.gz"
        )),
        "9d1d43a2-e1aa-4c16-acd4-3dd8a6a2fb85.json.gz" => Some(include_bytes!(
            "lcia_source_policy_assets_v2/9d1d43a2-e1aa-4c16-acd4-3dd8a6a2fb85.json.gz"
        )),
        "b2ad6110-c78d-11e6-9d9d-cec0c932ce01.json.gz" => Some(include_bytes!(
            "lcia_source_policy_assets_v2/b2ad6110-c78d-11e6-9d9d-cec0c932ce01.json.gz"
        )),
        "b2ad6494-c78d-11e6-9d9d-cec0c932ce01.json.gz" => Some(include_bytes!(
            "lcia_source_policy_assets_v2/b2ad6494-c78d-11e6-9d9d-cec0c932ce01.json.gz"
        )),
        "b2ad66ce-c78d-11e6-9d9d-cec0c932ce01.json.gz" => Some(include_bytes!(
            "lcia_source_policy_assets_v2/b2ad66ce-c78d-11e6-9d9d-cec0c932ce01.json.gz"
        )),
        "b2ad6890-c78d-11e6-9d9d-cec0c932ce01.json.gz" => Some(include_bytes!(
            "lcia_source_policy_assets_v2/b2ad6890-c78d-11e6-9d9d-cec0c932ce01.json.gz"
        )),
        "b53ec18f-7377-4ad3-86eb-cc3f4f276b2b.json.gz" => Some(include_bytes!(
            "lcia_source_policy_assets_v2/b53ec18f-7377-4ad3-86eb-cc3f4f276b2b.json.gz"
        )),
        "b5c602c6-def3-11e6-bf01-fe55135034f3.json.gz" => Some(include_bytes!(
            "lcia_source_policy_assets_v2/b5c602c6-def3-11e6-bf01-fe55135034f3.json.gz"
        )),
        "b5c610fe-def3-11e6-bf01-fe55135034f3.json.gz" => Some(include_bytes!(
            "lcia_source_policy_assets_v2/b5c610fe-def3-11e6-bf01-fe55135034f3.json.gz"
        )),
        "b5c611c6-def3-11e6-bf01-fe55135034f3.json.gz" => Some(include_bytes!(
            "lcia_source_policy_assets_v2/b5c611c6-def3-11e6-bf01-fe55135034f3.json.gz"
        )),
        "b5c614d2-def3-11e6-bf01-fe55135034f3.json.gz" => Some(include_bytes!(
            "lcia_source_policy_assets_v2/b5c614d2-def3-11e6-bf01-fe55135034f3.json.gz"
        )),
        "b5c619fa-def3-11e6-bf01-fe55135034f3.json.gz" => Some(include_bytes!(
            "lcia_source_policy_assets_v2/b5c619fa-def3-11e6-bf01-fe55135034f3.json.gz"
        )),
        "b5c629d6-def3-11e6-bf01-fe55135034f3.json.gz" => Some(include_bytes!(
            "lcia_source_policy_assets_v2/b5c629d6-def3-11e6-bf01-fe55135034f3.json.gz"
        )),
        "b5c632be-def3-11e6-bf01-fe55135034f3.json.gz" => Some(include_bytes!(
            "lcia_source_policy_assets_v2/b5c632be-def3-11e6-bf01-fe55135034f3.json.gz"
        )),
        "dacd48b5-4da5-49aa-aff4-cd5f5495c037.json.gz" => Some(include_bytes!(
            "lcia_source_policy_assets_v2/dacd48b5-4da5-49aa-aff4-cd5f5495c037.json.gz"
        )),
        "fd530f00-9325-424a-92ef-aaac67922fd9.json.gz" => Some(include_bytes!(
            "lcia_source_policy_assets_v2/fd530f00-9325-424a-92ef-aaac67922fd9.json.gz"
        )),
        _ => None,
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp)] // Source values are compared without numerical arithmetic.
mod tests {
    use super::*;

    use crate::{
        calculation_evidence::{
            LciaExchangeObservation, METHOD_SOURCE_SNAPSHOT_SCHEMA_VERSION, RELEASE_BUNDLE_VERSION,
            RELEASE_METHOD_IDENTITY_MANIFEST_SHA256, RELEASE_METHOD_MANIFEST_SHA256,
            RELEASE_SOURCE_SNAPSHOT_SHA256, STATIC_CACHE_BUNDLE_MANIFEST_PATH,
        },
        lcia_applicability::{DecisionStatus, MethodFactors},
        lcia_source_xml::inspect_ilcd_method,
        static_lcia_cache::{StaticLciaFactor, StaticLciaMethod},
    };

    const CLIMATE: &str = "7fce5b3a-66b8-4ce1-91e8-a925aee1f186";
    const ACIDIFICATION: &str = "b5c611c6-def3-11e6-bf01-fe55135034f3";
    const PM: &str = "b5c602c6-def3-11e6-bf01-fe55135034f3";
    const FRESHWATER: &str = "b53ec18f-7377-4ad3-86eb-cc3f4f276b2b";

    fn source(id: &str) -> InspectedIlcdMethod {
        let bytes: &[u8] = match id {
            CLIMATE => include_bytes!(
                "../tests/fixtures/lcia_supported_sources/7fce5b3a-66b8-4ce1-91e8-a925aee1f186.xml.gz"
            ),
            ACIDIFICATION => include_bytes!(
                "../tests/fixtures/lcia_supported_sources/b5c611c6-def3-11e6-bf01-fe55135034f3.xml.gz"
            ),
            PM => include_bytes!(
                "../tests/fixtures/lcia_supported_sources/b5c602c6-def3-11e6-bf01-fe55135034f3.xml.gz"
            ),
            FRESHWATER => include_bytes!(
                "../tests/fixtures/lcia_supported_sources/b53ec18f-7377-4ad3-86eb-cc3f4f276b2b.xml.gz"
            ),
            _ => panic!("unknown fixture"),
        };
        let mut xml = Vec::new();
        GzDecoder::new(bytes).read_to_end(&mut xml).unwrap();
        inspect_ilcd_method(&xml).unwrap()
    }

    fn observe(flow: &str, location: FactorLocation) -> LciaExchangeObservation {
        LciaExchangeObservation {
            flow_id: Uuid::parse_str(flow).unwrap(),
            flow_version: "03.00.004".to_owned(),
            location,
            direction: Some(LciaExchangeDirection::Output),
            direction_label: "Output".to_owned(),
            exchange_id: "unit-source-selection-observation".to_owned(),
            amount: Some(1.0),
        }
    }

    #[test]
    fn all_source_assets_have_exact_counts_and_explicitly_bounded_default_coverage() {
        let manifest = manifest().unwrap();
        let mut count = 0;
        let mut unresolved = 0;
        for entry in &manifest.methods {
            let companion = decode_companion(entry).unwrap();
            count += companion.bindings.len();
            unresolved += companion.regional_default_unresolved_family_count;
            for index in &companion.generic_records {
                assert!(matches!(
                    companion.locations[companion.bindings[*index].3],
                    FactorLocation::Blank | FactorLocation::Missing
                ));
            }
        }
        assert_eq!(count, 319_575);
        assert_eq!(unresolved, 227);
    }

    #[test]
    fn original_climate_source_needs_no_operator_proof_or_exchange_location() {
        let source = source(CLIMATE);
        let factors = MethodFactors::from_native_source(&source, source.method_id).unwrap();
        let flow = "08a91e70-3ddc-11dd-923d-0050c2490048";
        for location in [
            FactorLocation::Missing,
            FactorLocation::Blank,
            FactorLocation::Unknown,
            FactorLocation::Code("CN".to_owned()),
        ] {
            let decision = factors.select(&observe(flow, location.clone()));
            assert_eq!(decision.status, DecisionStatus::Selected);
            assert_eq!(decision.reason, "reviewed_source_generic_default");
            assert_eq!(decision.location, location);
            let selected = decision.selected.unwrap();
            assert_eq!(selected.value, 1.0);
            assert_eq!(selected.location, FactorLocation::Blank);
            assert!(decision.source.nonspatial_proof.is_none());
            let receipt = decision.source.source_policy.unwrap();
            assert_eq!(receipt.source_xml_sha256, source.raw_sha256);
            assert_eq!(receipt.source_binding_kind, "exact_raw_ilcd_xml");
        }
        let from_document = MethodFactors::from_method_document(
            source.method_id,
            &source.method_version,
            source.method_id,
            &source.method_document,
        )
        .unwrap();
        assert_eq!(
            from_document
                .select(&observe(flow, FactorLocation::Missing))
                .status,
            DecisionStatus::Selected
        );
        for (flow, value) in [
            ("08a91e70-3ddc-11dd-94c3-0050c2490048", 273.0),
            ("08a91e70-3ddc-11dd-9610-0050c2490048", 29.8),
        ] {
            for location in [
                FactorLocation::Missing,
                FactorLocation::Code("CN".to_owned()),
            ] {
                let decision = factors.select(&observe(flow, location));
                assert_eq!(decision.status, DecisionStatus::Selected);
                assert_eq!(decision.selected.unwrap().value, value);
            }
        }
    }

    #[test]
    fn original_acidification_prefers_de_and_uses_explicit_reviewed_generic() {
        let source = source(ACIDIFICATION);
        let factors = MethodFactors::from_native_source(&source, source.method_id).unwrap();
        let flow = "08a91e70-3ddc-11dd-a2a9-0050c2490048";
        for (location, expected, record_index) in [
            (FactorLocation::Code("RER".to_owned()), 3.02, 211),
            (FactorLocation::Code("DE".to_owned()), 4.0, 574),
        ] {
            let decision = factors.select(&observe(flow, location));
            assert_eq!(decision.status, DecisionStatus::Selected);
            let selected = decision.selected.unwrap();
            assert_eq!(selected.value, expected);
            assert_eq!(selected.record_index, record_index);
            assert_ne!(selected.value, 7.02);
            assert!(
                decision
                    .rejected
                    .iter()
                    .all(|rejected| rejected.factor.record_index != selected.record_index)
            );
        }
        for location in [
            FactorLocation::Missing,
            FactorLocation::Blank,
            FactorLocation::Unknown,
            FactorLocation::Code("BR".to_owned()),
            FactorLocation::Code("CN".to_owned()),
            FactorLocation::Code("unrecognized-region".to_owned()),
            FactorLocation::Code("DE-unreviewed-alias".to_owned()),
        ] {
            let decision = factors.select(&observe(flow, location));
            assert_eq!(decision.status, DecisionStatus::Unsupported);
            assert_eq!(decision.reason, "lcia_generic_location_not_admitted");
            assert!(decision.selected.is_none());
            assert!(
                decision
                    .rejected
                    .iter()
                    .any(|record| record.factor.record_index == 211)
            );
        }
        let mut wrong_version = observe(flow, FactorLocation::Missing);
        wrong_version.flow_version = "03.00.003".to_owned();
        let decision = factors.select(&wrong_version);
        assert_eq!(decision.status, DecisionStatus::Unsupported);
        assert_eq!(decision.reason, "lcia_factor_flow_version_mismatch");
        let mut wrong_direction = observe(flow, FactorLocation::Missing);
        wrong_direction.direction = Some(LciaExchangeDirection::Input);
        assert_ne!(
            factors.select(&wrong_direction).status,
            DecisionStatus::Selected
        );
    }

    #[test]
    fn geographic_admission_binds_exact_source_geography_without_country_inference() {
        let source_scope = serde_json::json!({"information":{"geography":{
            "interventionLocation":"RER","impactLocation":"RER"}}});
        let geographic_hash =
            canonical_json_sha256(&source_scope["information"]["geography"]).unwrap();
        let scoped = GenericGeographicAdmission::ExactSourceTokens {
            source_geography_sha256: geographic_hash.clone(),
            tokens: ["RER".to_owned()].into(),
        };
        scoped.validate_source(&source_scope).unwrap();
        assert!(scoped.admits(&FactorLocation::Code("RER".to_owned())));
        for location in [
            FactorLocation::Missing,
            FactorLocation::Blank,
            FactorLocation::Unknown,
            FactorLocation::Invalid,
            FactorLocation::Code("DE".to_owned()),
            FactorLocation::Code("BR".to_owned()),
        ] {
            assert!(!scoped.admits(&location));
        }
        for declaration in [
            GenericGeographicAdmission::Global {
                source_geography_sha256: geographic_hash.clone(),
            },
            GenericGeographicAdmission::ExactSourceTokens {
                source_geography_sha256: geographic_hash.clone(),
                tokens: ["CN".to_owned()].into(),
            },
            GenericGeographicAdmission::ExactSourceTokens {
                source_geography_sha256: geographic_hash,
                tokens: ["RER".to_owned(), "DE".to_owned()].into(),
            },
        ] {
            assert!(declaration.validate_source(&source_scope).is_err());
        }
        assert!(
            scoped
                .validate_source(&serde_json::json!({"information":{}}))
                .is_err()
        );
        let global_scope =
            serde_json::json!({"information":{"geography":{"interventionLocation":"GLO"}}});
        let global = GenericGeographicAdmission::Global {
            source_geography_sha256: canonical_json_sha256(
                &global_scope["information"]["geography"],
            )
            .unwrap(),
        };
        global.validate_source(&global_scope).unwrap();
        assert!(global.admits(&FactorLocation::Missing));
        assert!(global.admits(&FactorLocation::Code("CN".to_owned())));
        assert!(!global.admits(&FactorLocation::Invalid));
    }

    #[test]
    fn a_nonspatial_proof_cannot_expand_a_known_european_source_domain() {
        use crate::lcia_applicability_proof::{ProofSource, TrustedProofSet};
        let source = source(FRESHWATER);
        let mut factors = MethodFactors::from_native_source(&source, source.method_id).unwrap();
        let basis = source
            .metadata_index
            .iter()
            .find(|entry| entry.xml_path.ends_with("}geography"))
            .unwrap();
        let document = serde_json::json!({"schema_version":"lcia.applicability-proof-set.v1","records":[{
            "method_id":source.method_id,"method_version":source.method_version,
            "artifact_locator_id":source.method_id,"decision":"location_independent",
            "source_raw_sha256":source.raw_sha256,"canonical_body_sha256":source.canonical_body_sha256,
            "ordered_factors_sha256":source.ordered_factors_sha256,"projection_schema":crate::lcia_source_xml::XML_PROJECTION_SCHEMA_VERSION,
            "basis":[{"pointer":basis.pointer,"value_sha256":basis.sha256}],
            "review_reference":{"decision_id":"synthetic-local-nonspatial-regression-not-proxy-adoption","sha256":"0".repeat(64)}
        }]});
        let directory = tempfile::tempdir().unwrap();
        let path = directory
            .path()
            .join("synthetic-scope-regression-proof.json");
        let bytes = serde_json::to_vec(&document).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        let proofs = TrustedProofSet::load(&path, &sha256_bytes(&bytes)).unwrap();
        let proof = proofs
            .bind_location_independent(ProofSource {
                method_id: source.method_id,
                method_version: &source.method_version,
                artifact_locator_id: source.method_id,
                source_raw_sha256: &source.raw_sha256,
                canonical_body_sha256: &source.canonical_body_sha256,
                ordered_factors_sha256: &source.ordered_factors_sha256,
                projection_schema: crate::lcia_source_xml::XML_PROJECTION_SCHEMA_VERSION,
                projection: &source.projection,
            })
            .unwrap()
            .unwrap();
        factors.apply_location_independent_proof(proof).unwrap();
        assert!(factors.is_location_independent());
        let first = &factors.records[0];
        for location in [
            FactorLocation::Missing,
            FactorLocation::Blank,
            FactorLocation::Unknown,
            FactorLocation::Code("BR".to_owned()),
            FactorLocation::Code("CN".to_owned()),
        ] {
            let observation = LciaExchangeObservation {
                flow_id: first.flow_id,
                flow_version: first.flow_version.clone(),
                location,
                direction: Some(first.direction),
                direction_label: first.direction.as_str().to_owned(),
                exchange_id: "synthetic-geo-proof-observation".to_owned(),
                amount: Some(1.0),
            };
            let decision = factors.select(&observation);
            assert_eq!(decision.status, DecisionStatus::Unsupported);
            assert_eq!(decision.reason, "lcia_generic_location_not_admitted");
            assert!(decision.selected.is_none());
        }
    }

    #[test]
    fn explicit_zero_factor_and_negative_amount_keep_the_source_selection_receipt() {
        let source = source(CLIMATE);
        let factors = MethodFactors::from_native_source(&source, source.method_id).unwrap();
        let mut observation = observe(
            "e99843b7-e598-447a-ab73-d01f179f0f8c",
            FactorLocation::Missing,
        );
        observation.flow_version = "01.00.002".to_owned();
        observation.direction = Some(LciaExchangeDirection::Input);
        observation.amount = Some(-1.0);
        let decision = factors.select(&observation);
        assert_eq!(decision.status, DecisionStatus::Selected);
        assert_eq!(decision.selected.unwrap().value, 0.0);
        assert!(decision.source.source_policy.is_some());
    }

    #[test]
    fn same_source_identity_with_changed_values_or_metadata_cannot_reuse_the_policy() {
        let source = source(CLIMATE);
        let flow = "08a91e70-3ddc-11dd-923d-0050c2490048";
        for pointer in [
            "/LCIAMethodDataSet/characterisationFactors/factor/954/meanValue",
            "/LCIAMethodDataSet/LCIAMethodInformation/geography/geographicalRepresentativenessDescription/#text",
            "/LCIAMethodDataSet/characterisationFactors/factor/954/generalComment",
        ] {
            let mut changed = source.method_document.clone();
            *changed.pointer_mut(pointer).expect("actual source pointer") =
                Value::String("unreviewed edit".to_owned());
            let result = MethodFactors::from_method_document(
                source.method_id,
                &source.method_version,
                source.method_id,
                &changed,
            );
            if let Ok(factors) = result {
                assert!(factors.source.source_policy.is_none());
                assert_ne!(
                    factors
                        .select(&observe(flow, FactorLocation::Missing))
                        .status,
                    DecisionStatus::Selected
                );
            }
        }
    }

    fn static_method_fixture(source: &InspectedIlcdMethod) -> VerifiedStaticLciaBundle {
        let entry = method_entry(source.method_id, &source.method_version, source.method_id)
            .unwrap()
            .unwrap();
        let companion = decode_companion(entry).unwrap();
        let parsed = MethodFactors::from_native_source(source, source.method_id).unwrap();
        let factors = companion
            .bindings
            .iter()
            .zip(parsed.records)
            .map(|(binding, record)| StaticLciaFactor {
                flow_id: record.flow_id,
                flow_version: record.flow_version,
                direction: match record.direction {
                    LciaExchangeDirection::Input => StaticLciaDirection::Input,
                    LciaExchangeDirection::Output => StaticLciaDirection::Output,
                },
                value: record.value,
                source_index_key: binding.0.clone(),
                source_factor_index: binding.1,
                source_record_sha256: "fixture-only: verified loader owns the original index hash"
                    .to_owned(),
            })
            .collect();
        VerifiedStaticLciaBundle {
            source_evidence: crate::calculation_evidence::LcaMethodFactorSourceSnapshot {
                schema_version: METHOD_SOURCE_SNAPSHOT_SCHEMA_VERSION.to_owned(),
                source_kind: "static_cache_bundle".to_owned(),
                bundle_manifest_path: STATIC_CACHE_BUNDLE_MANIFEST_PATH.to_owned(),
                bundle_manifest_sha256: RELEASE_BUNDLE_MANIFEST_SHA256.to_owned(),
                bundle_version: RELEASE_BUNDLE_VERSION.to_owned(),
                source_snapshot_sha256: RELEASE_SOURCE_SNAPSHOT_SHA256.to_owned(),
                method_manifest_sha256: RELEASE_METHOD_MANIFEST_SHA256.to_owned(),
                factor_manifest_sha256: RELEASE_FACTOR_MANIFEST_SHA256.to_owned(),
                method_identity_manifest_sha256: RELEASE_METHOD_IDENTITY_MANIFEST_SHA256.to_owned(),
                method_count: 25,
            },
            methods: vec![StaticLciaMethod {
                method_id: source.method_id,
                method_version: source.method_version.clone(),
                artifact_locator_id: source.method_id,
                artifact_filename: "fixture".to_owned(),
                name: "Original source selection fixture".to_owned(),
                unit: "source unit".to_owned(),
            }],
            factors_by_method: [(source.method_id, factors)].into(),
        }
    }

    #[test]
    fn shipped_companion_recovers_static_applicability_and_refuses_numeric_or_version_drift() {
        let source = source(ACIDIFICATION);
        let bundle = static_method_fixture(&source);
        let restored = MethodFactors::from_static_bundle(&bundle, source.method_id).unwrap();
        assert_eq!(restored.records.len(), 935);
        assert!(restored.source.applicability_complete);
        for (location, expected) in [
            (FactorLocation::Code("RER".to_owned()), 3.02),
            (FactorLocation::Code("DE".to_owned()), 4.0),
        ] {
            let decision =
                restored.select(&observe("08a91e70-3ddc-11dd-a2a9-0050c2490048", location));
            assert_eq!(decision.status, DecisionStatus::Selected);
            assert_eq!(decision.selected.unwrap().value, expected);
        }
        for location in [
            FactorLocation::Missing,
            FactorLocation::Blank,
            FactorLocation::Unknown,
            FactorLocation::Code("BR".to_owned()),
            FactorLocation::Code("CN".to_owned()),
            FactorLocation::Code("unrecognized-region".to_owned()),
        ] {
            let decision =
                restored.select(&observe("08a91e70-3ddc-11dd-a2a9-0050c2490048", location));
            assert_eq!(decision.status, DecisionStatus::Unsupported);
            assert_eq!(decision.reason, "lcia_generic_location_not_admitted");
            assert!(decision.selected.is_none());
        }
        for mutate in ["value", "version", "index", "missing"] {
            let mut changed = bundle.clone();
            let factors = changed
                .factors_by_method
                .get_mut(&source.method_id)
                .unwrap();
            match mutate {
                "value" => factors[211].value = 123.0,
                "version" => factors[211].flow_version = "03.00.003".to_owned(),
                "index" => factors[211].source_factor_index += 1,
                "missing" => {
                    factors.pop();
                }
                _ => unreachable!(),
            }
            assert!(
                MethodFactors::from_static_bundle(&changed, source.method_id).is_err(),
                "{mutate} drift must fail before source applicability is admitted"
            );
        }
        let mut unreviewed_release = bundle;
        "0".repeat(64)
            .clone_into(&mut unreviewed_release.source_evidence.bundle_manifest_sha256);
        let retained =
            MethodFactors::from_static_bundle(&unreviewed_release, source.method_id).unwrap();
        assert!(!retained.source.applicability_complete);
        assert!(retained.source.source_policy.is_none());
    }

    #[test]
    fn pm_flow_archetype_comments_are_retained_and_regional_default_remains_unreviewed() {
        let source = source(PM);
        let factors = MethodFactors::from_native_source(&source, source.method_id).unwrap();
        let entry = method_entry(source.method_id, &source.method_version, source.method_id)
            .unwrap()
            .unwrap();
        let companion = decode_companion(entry).unwrap();
        assert_eq!(companion.reviewed_comment_records.len(), 306);
        assert_eq!(companion.regional_default_unresolved_family_count, 25);
        for index in companion.reviewed_comment_records {
            let record = &factors.records[index];
            assert!(!record.unsupported_qualifiers);
            let comment = &record.qualifiers["generalComment"];
            assert!(
                !comment
                    .as_str()
                    .or_else(|| comment.get("#text")?.as_str())
                    .unwrap()
                    .is_empty()
            );
        }
    }
}
