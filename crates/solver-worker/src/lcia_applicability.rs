//! Source-bound LCIA factor selection. Compilation and offline diagnosis use the same policy.
//!
//! This policy supports literal location tokens and explicitly pinned, source-bound reviewed
//! location-independent claims. Missing, blank and omitted applicability remain distinct.
//! No geography or version fallback is inferred. The existing one-direction-per-Elementary-Flow
//! constraint remains with callers.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::calculation_evidence::{
    LciaExchangeDirection, LciaExchangeObservation, canonical_json_sha256,
};
use crate::lcia_applicability_proof::{ProofReceipt, ValidatedNonspatialProof};
use crate::lcia_source_policy::{SourcePolicyReceipt, SupportedSourcePolicy};
use crate::lcia_source_xml::{
    InspectedIlcdMethod, XML_CANONICALIZATION, XML_PROJECTION_SCHEMA_VERSION,
};
use crate::static_lcia_cache::VerifiedStaticLciaBundle;

pub const POLICY_VERSION: &str = "lcia.applicability.source-bound.v3";
pub const EVIDENCE_SCHEMA_VERSION: &str = "lcia.applicability.evidence.v2";
pub const MAX_DIAGNOSTIC_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_SELECTION_RECORDS: usize = 100_000;
const MAX_FACTOR_RECORDS: usize = 1_000_000;
const MAX_FACTOR_RECORD_BYTES: usize = 1024 * 1024;
const MAX_FLOW_CANDIDATES: usize = 4_096;

#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum FactorLocation {
    /// Older evidence/index formats did not retain the field; absence cannot be reconstructed.
    #[default]
    Unknown,
    /// The complete source record has no location field.
    Missing,
    /// The complete source record explicitly contains an empty string.
    Blank,
    /// An exact source token. GLO is a token, not a fallback rule.
    Code(String),
    /// Malformed/unsupported encodings remain distinct from an absent field.
    Invalid,
}

impl FactorLocation {
    #[must_use]
    pub fn from_source(value: Option<&Value>) -> Self {
        match value {
            None => Self::Missing,
            Some(Value::String(value)) if value.trim().is_empty() => Self::Blank,
            Some(Value::String(value)) if value.len() <= 256 => Self::Code(value.clone()),
            _ => Self::Invalid,
        }
    }

    #[must_use]
    pub fn is_exact_token(&self) -> bool {
        matches!(self, Self::Code(value) if !value.trim().is_empty() && value.len() <= 256)
    }
}

#[derive(Debug, Clone, thiserror::Error)]
#[error("{code}: {message}")]
pub struct ApplicabilityError {
    pub code: &'static str,
    pub message: String,
    pub details: Value,
}

impl ApplicabilityError {
    #[must_use]
    pub fn new(code: &'static str, message: impl Into<String>, details: Value) -> Self {
        Self {
            code,
            message: message.into(),
            details,
        }
    }

    #[must_use]
    pub fn issue(&self) -> Value {
        json!({"code": self.code, "message": self.message, "details": self.details})
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FactorRecord {
    pub record_index: usize,
    pub record_locator: String,
    pub record_sha256: String,
    pub flow_id: Uuid,
    pub flow_version: String,
    pub direction: LciaExchangeDirection,
    pub location: FactorLocation,
    pub value: f64,
    /// Source qualifiers are retained verbatim; their presence never grants applicability.
    pub qualifiers: Value,
    pub unsupported_qualifiers: bool,
    pub unresolved_qualifier_fields: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeSourceReceipt {
    pub raw_sha256: String,
    pub canonical_body_sha256: String,
    pub ordered_factors_sha256: String,
    pub projection_schema: String,
    pub canonicalization: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactorSource {
    pub method_id: Uuid,
    pub method_version: String,
    pub artifact_locator_id: Uuid,
    pub source_sha256: String,
    pub source_kind: String,
    pub factor_count: usize,
    pub applicability_complete: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_source: Option<NativeSourceReceipt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nonspatial_proof: Option<ProofReceipt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_policy: Option<SourcePolicyReceipt>,
}

#[derive(Debug, Clone)]
pub struct MethodFactors {
    pub source: FactorSource,
    pub records: Vec<FactorRecord>,
    by_flow: BTreeMap<Uuid, Vec<usize>>,
    nonspatial_proof: Option<ValidatedNonspatialProof>,
    source_policy: Option<SupportedSourcePolicy>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionStatus {
    Selected,
    Unmatched,
    Unsupported,
    Invalid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RejectedFactor {
    pub factor: FactorRecord,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FactorDecision {
    pub policy_version: String,
    pub source: FactorSource,
    pub flow_id: Uuid,
    pub flow_version: String,
    pub direction: Option<LciaExchangeDirection>,
    pub location: FactorLocation,
    pub status: DecisionStatus,
    pub reason: String,
    pub selected: Option<FactorRecord>,
    pub rejected: Vec<RejectedFactor>,
}

fn unselected_status(
    exact_version: bool,
    geographic_refusal: bool,
    unresolved_location: bool,
) -> (DecisionStatus, &'static str) {
    match (exact_version, geographic_refusal, unresolved_location) {
        (false, _, _) => (
            DecisionStatus::Unsupported,
            "lcia_factor_flow_version_mismatch",
        ),
        (_, true, _) => (
            DecisionStatus::Unsupported,
            "lcia_generic_location_not_admitted",
        ),
        (_, _, true) => (
            DecisionStatus::Unsupported,
            "lcia_factor_location_semantics_unresolved",
        ),
        _ => (
            DecisionStatus::Unmatched,
            "no_lcia_factor_for_exact_location_direction",
        ),
    }
}

// An empty comment carries no condition. Language/uncertainty metadata remain preserved;
// nonempty comments and unclassified extension fields require explicit applicability review.
fn empty_comment(value: &Value) -> bool {
    match value {
        Value::String(text) => text.trim().is_empty(),
        Value::Array(items) => items.iter().all(empty_comment),
        Value::Object(fields) => {
            fields
                .keys()
                .all(|key| matches!(key.as_str(), "@xml:lang" | "#text"))
                && fields.get("#text").is_none_or(empty_comment)
        }
        _ => false,
    }
}

fn parse_method_factor(
    method_id: Uuid,
    method_version: &str,
    source_sha256: &str,
    record_index: usize,
    item: &Value,
    is_array: bool,
) -> anyhow::Result<FactorRecord> {
    let invalid = |field: &str| {
        ApplicabilityError::new(
            "lcia_factor_record_invalid",
            format!("invalid {field} in source factor"),
            json!({"method_id":method_id,"method_version":method_version,
                    "source_sha256":source_sha256,"record_index":record_index,"field":field}),
        )
    };
    let flow_id = item
        .pointer("/referenceToFlowDataSet/@refObjectId")
        .and_then(Value::as_str)
        .and_then(|value| Uuid::parse_str(value).ok())
        .ok_or_else(|| invalid("flow_id"))?;
    if let Some(reference_type) = item.pointer("/referenceToFlowDataSet/@type")
        && reference_type.as_str() != Some("flow data set")
    {
        return Err(invalid("flow_reference_type").into());
    }
    let flow_version = item
        .pointer("/referenceToFlowDataSet/@version")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty() && value.len() <= 128)
        .ok_or_else(|| invalid("flow_version"))?
        .to_owned();
    if crate::calculation_bundle::validate_version(&flow_version, "lcia_factor.flow_version")
        .is_err()
    {
        return Err(invalid("flow_version").into());
    }
    anyhow::ensure!(
        serde_json::to_vec(item)?.len() <= MAX_FACTOR_RECORD_BYTES,
        "lcia_factor_record_size_limit"
    );
    let direction = match item.get("exchangeDirection").and_then(Value::as_str) {
        Some(value) if value.eq_ignore_ascii_case("input") => LciaExchangeDirection::Input,
        Some(value) if value.eq_ignore_ascii_case("output") => LciaExchangeDirection::Output,
        _ => return Err(invalid("exchange_direction").into()),
    };
    let value = match item.get("meanValue") {
        Some(Value::Number(value)) => value.as_f64(),
        Some(Value::String(value)) => value.parse::<f64>().ok(),
        _ => None,
    }
    .filter(|value| value.is_finite())
    .ok_or_else(|| invalid("mean_value"))?;
    let location = FactorLocation::from_source(item.get("location"));
    if location == FactorLocation::Invalid {
        return Err(invalid("location").into());
    }
    let qualifiers = item
        .as_object()
        .ok_or_else(|| invalid("factor"))?
        .iter()
        .filter(|(key, _)| {
            !matches!(
                key.as_str(),
                "referenceToFlowDataSet" | "exchangeDirection" | "location" | "meanValue"
            )
        })
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect::<serde_json::Map<_, _>>();
    let unresolved_qualifier_fields = qualifiers
        .iter()
        .filter(|(key, value)| {
            !(matches!(
                key.as_str(),
                "minimumValue"
                    | "maximumValue"
                    | "uncertaintyDistributionType"
                    | "relativeStandardDeviation95In"
                    | "dataDerivationTypeStatus"
                    | "deviatingRecommendation"
                    | "referencesToDataSource"
            ) || key.as_str() == "generalComment" && empty_comment(value))
        })
        .map(|(key, _)| key.clone())
        .collect::<Vec<_>>();
    let unsupported_qualifiers = !unresolved_qualifier_fields.is_empty();
    let record_locator = if is_array {
        format!("/LCIAMethodDataSet/characterisationFactors/factor/{record_index}")
    } else {
        "/LCIAMethodDataSet/characterisationFactors/factor".to_owned()
    };
    Ok(FactorRecord {
        record_index,
        record_locator,
        record_sha256: canonical_json_sha256(item)?,
        flow_id,
        flow_version,
        direction,
        location,
        value,
        qualifiers: Value::Object(qualifiers),
        unsupported_qualifiers,
        unresolved_qualifier_fields,
    })
}

impl MethodFactors {
    pub fn from_method_document(
        method_id: Uuid,
        method_version: &str,
        artifact_locator_id: Uuid,
        document: &Value,
    ) -> anyhow::Result<Self> {
        let mut factors = Self::from_method_document_unbound(
            method_id,
            method_version,
            artifact_locator_id,
            document,
        )?;
        if let Some(policy) = crate::lcia_source_policy::bind_method_document(
            method_id,
            method_version,
            artifact_locator_id,
            &factors.source.source_sha256,
        )? {
            factors.attach_source_policy(policy);
        }
        Ok(factors)
    }

    fn from_method_document_unbound(
        method_id: Uuid,
        method_version: &str,
        artifact_locator_id: Uuid,
        document: &Value,
    ) -> anyhow::Result<Self> {
        crate::calculation_bundle::validate_version(method_version, "lcia_method.version")?;
        anyhow::ensure!(
            u64::try_from(serde_json::to_vec(document)?.len())? <= MAX_DIAGNOSTIC_BYTES,
            "lcia_method_document_size_limit"
        );
        let source_sha256 = canonical_json_sha256(document)?;
        let factor_container =
            document.pointer("/LCIAMethodDataSet/characterisationFactors/factor");
        let items: Vec<&Value> = match factor_container {
            None => anyhow::bail!("lcia_factor_records_missing"),
            Some(Value::Array(items)) => items.iter().collect(),
            Some(item @ Value::Object(_)) => vec![item],
            Some(_) => anyhow::bail!("lcia_factor_records_malformed"),
        };
        anyhow::ensure!(
            items.len() <= MAX_FACTOR_RECORDS,
            "lcia_factor_record_count_limit"
        );
        let mut records = Vec::with_capacity(items.len());
        for (record_index, item) in items.into_iter().enumerate() {
            records.push(parse_method_factor(
                method_id,
                method_version,
                &source_sha256,
                record_index,
                item,
                factor_container.is_some_and(Value::is_array),
            )?);
        }
        Ok(Self::new(
            FactorSource {
                method_id,
                method_version: method_version.to_owned(),
                artifact_locator_id,
                source_sha256,
                source_kind: "complete_method_document".to_owned(),
                factor_count: records.len(),
                applicability_complete: true,
                native_source: None,
                nonspatial_proof: None,
                source_policy: None,
            },
            records,
        ))
    }

    /// Read-only native projection and adapter originate from the same bounded XML reader.
    pub fn from_native_source(
        source: &InspectedIlcdMethod,
        artifact_locator_id: Uuid,
    ) -> anyhow::Result<Self> {
        let mut factors = Self::from_method_document_unbound(
            source.method_id,
            &source.method_version,
            artifact_locator_id,
            &source.method_document,
        )?;
        anyhow::ensure!(
            factors.records.len() == source.ordered_factors.len(),
            "lcia_native_factor_cardinality_mismatch"
        );
        for (record, original) in factors.records.iter_mut().zip(&source.ordered_factors) {
            record.record_sha256 = canonical_json_sha256(original)?;
            record.record_locator = format!("ordered_factors/{}", record.record_index);
        }
        "primary_ilcd_xml".clone_into(&mut factors.source.source_kind);
        factors
            .source
            .source_sha256
            .clone_from(&source.canonical_body_sha256);
        factors.source.native_source = Some(NativeSourceReceipt {
            raw_sha256: source.raw_sha256.clone(),
            canonical_body_sha256: source.canonical_body_sha256.clone(),
            ordered_factors_sha256: source.ordered_factors_sha256.clone(),
            projection_schema: XML_PROJECTION_SCHEMA_VERSION.to_owned(),
            canonicalization: XML_CANONICALIZATION.to_owned(),
        });
        if let Some(policy) =
            crate::lcia_source_policy::bind_native_source(source, artifact_locator_id)?
        {
            factors.attach_source_policy(policy);
        }
        Ok(factors)
    }

    pub fn apply_location_independent_proof(
        &mut self,
        proof: ValidatedNonspatialProof,
    ) -> anyhow::Result<()> {
        self.ensure_source_usable()?;
        let receipt = proof.receipt();
        let native =
            self.source.native_source.as_ref().ok_or_else(|| {
                anyhow::anyhow!("lcia_nonspatial_requires_complete_native_source")
            })?;
        anyhow::ensure!(
            receipt.method_id == self.source.method_id
                && receipt.method_version == self.source.method_version
                && receipt.artifact_locator_id == self.source.artifact_locator_id
                && receipt.source_raw_sha256 == native.raw_sha256
                && receipt.canonical_body_sha256 == native.canonical_body_sha256
                && receipt.ordered_factors_sha256 == native.ordered_factors_sha256
                && receipt.projection_schema == native.projection_schema,
            "lcia_nonspatial_proof_source_mismatch"
        );
        let mut keys = BTreeSet::new();
        for factor in &self.records {
            let reason = if !matches!(
                factor.location,
                FactorLocation::Missing | FactorLocation::Blank
            ) {
                Some("named or unknown factor location contradicts the location-independent proof")
            } else if factor.unsupported_qualifiers {
                Some("factor-specific conditions or unclassified fields require review")
            } else if !keys.insert((
                factor.flow_id,
                &factor.flow_version,
                factor.direction.as_str(),
            )) {
                Some("multiple factors remain for the same exact Flow/version/direction")
            } else {
                None
            };
            if let Some(reason) = reason {
                return Err(ApplicabilityError::new(
                    "lcia_nonspatial_source_conflict",
                    reason,
                    json!({"source":self.source,"factor":factor}),
                )
                .into());
            }
        }
        self.source.nonspatial_proof = Some(receipt);
        self.nonspatial_proof = Some(proof);
        Ok(())
    }

    #[must_use]
    pub fn is_location_independent(&self) -> bool {
        self.nonspatial_proof.is_some()
    }

    pub fn from_static_bundle(
        bundle: &VerifiedStaticLciaBundle,
        method_id: Uuid,
    ) -> anyhow::Result<Self> {
        let method = bundle
            .methods
            .iter()
            .find(|method| method.method_id == method_id)
            .ok_or_else(|| anyhow::anyhow!("unmanifested LCIA method"))?;
        if let Some((records, policy)) = crate::lcia_source_policy::complete_static_method(
            bundle,
            method_id,
            &method.method_version,
            method.artifact_locator_id,
        )? {
            let mut factors = Self::new(
                FactorSource {
                    method_id,
                    method_version: method.method_version.clone(),
                    artifact_locator_id: method.artifact_locator_id,
                    source_sha256: bundle.source_evidence.factor_manifest_sha256.clone(),
                    source_kind: "supported_static_index_with_applicability_companion".to_owned(),
                    factor_count: records.len(),
                    applicability_complete: true,
                    native_source: None,
                    nonspatial_proof: None,
                    source_policy: None,
                },
                records,
            );
            factors.attach_source_policy(policy);
            return Ok(factors);
        }
        // Unrecognized legacy inputs retain integrity/read compatibility, without invented
        // applicability. The reviewed release instead uses its shipped companion above.
        let factor_count = bundle.factors_by_method.get(&method_id).map_or(0, Vec::len);
        Ok(Self::new(
            FactorSource {
                method_id,
                method_version: method.method_version.clone(),
                artifact_locator_id: method.artifact_locator_id,
                source_sha256: bundle.source_evidence.factor_manifest_sha256.clone(),
                source_kind: "legacy_static_index".to_owned(),
                factor_count,
                applicability_complete: false,
                native_source: None,
                nonspatial_proof: None,
                source_policy: None,
            },
            Vec::new(),
        ))
    }

    fn new(source: FactorSource, records: Vec<FactorRecord>) -> Self {
        let mut by_flow = BTreeMap::<_, Vec<_>>::new();
        for (index, record) in records.iter().enumerate() {
            by_flow.entry(record.flow_id).or_default().push(index);
        }
        Self {
            source,
            records,
            by_flow,
            nonspatial_proof: None,
            source_policy: None,
        }
    }

    fn attach_source_policy(&mut self, policy: SupportedSourcePolicy) {
        policy.review_qualifiers(&mut self.records);
        self.source.source_policy = Some(policy.receipt.clone());
        self.source_policy = Some(policy);
    }

    pub fn ensure_source_usable(&self) -> Result<(), ApplicabilityError> {
        if self.source.applicability_complete {
            return Ok(());
        }
        Err(ApplicabilityError::new(
            "lcia_applicability_source_incomplete",
            "the verified static index omits location and qualifier evidence; its factors cannot authorize characterization",
            serde_json::to_value(&self.source).expect("finite source metadata"),
        ))
    }

    fn input_refusal(
        &self,
        observation: &LciaExchangeObservation,
        candidate_count: usize,
    ) -> Option<(DecisionStatus, &'static str)> {
        let unsupported = DecisionStatus::Unsupported;
        if !self.source.applicability_complete {
            Some((unsupported, "lcia_applicability_source_incomplete"))
        } else if observation.direction.is_none() {
            Some((unsupported, "unsupported_exchange_direction"))
        } else if crate::calculation_bundle::validate_version(
            &observation.flow_version,
            "inventory.flow_version",
        )
        .is_err()
        {
            Some((unsupported, "lcia_inventory_flow_version_invalid"))
        } else if observation.amount.is_none_or(|value| !value.is_finite()) {
            Some((DecisionStatus::Invalid, "invalid_exchange_amount"))
        } else if candidate_count > MAX_FLOW_CANDIDATES {
            Some((unsupported, "lcia_factor_candidate_limit_exceeded"))
        } else if candidate_count == 0 {
            Some((
                DecisionStatus::Unmatched,
                "no_lcia_factor_for_flow_direction",
            ))
        } else if observation.location == FactorLocation::Invalid {
            Some((unsupported, "lcia_exchange_location_invalid"))
        } else if !self.is_location_independent()
            && self.source_policy.is_none()
            && !observation.location.is_exact_token()
        {
            Some((unsupported, "lcia_exchange_location_semantics_unresolved"))
        } else {
            None
        }
    }

    fn pending_decision(&self, observation: &LciaExchangeObservation) -> FactorDecision {
        FactorDecision {
            policy_version: POLICY_VERSION.to_owned(),
            source: self.source.clone(),
            flow_id: observation.flow_id,
            flow_version: observation.flow_version.clone(),
            direction: observation.direction,
            location: observation.location.clone(),
            status: DecisionStatus::Unsupported,
            reason: String::new(),
            selected: None,
            rejected: vec![],
        }
    }

    fn record_selection(
        &self,
        decision: &mut FactorDecision,
        factor: &FactorRecord,
        used_generic: bool,
    ) {
        decision.status = DecisionStatus::Selected;
        (if self.is_location_independent() {
            "reviewed_location_independent_source"
        } else if used_generic {
            "reviewed_source_generic_default"
        } else {
            "exact_source_location_token"
        })
        .clone_into(&mut decision.reason);
        decision.selected = Some(factor.clone());
        decision
            .rejected
            .retain(|item| item.factor.record_index != factor.record_index);
    }

    #[must_use]
    pub fn select(&self, observation: &LciaExchangeObservation) -> FactorDecision {
        let mut decision = self.pending_decision(observation);
        let candidates = self
            .by_flow
            .get(&observation.flow_id)
            .map_or(&[][..], Vec::as_slice);
        if let Some((status, reason)) = self.input_refusal(observation, candidates.len()) {
            decision.status = status;
            reason.clone_into(&mut decision.reason);
            return decision;
        }
        let mut exact = Vec::new();
        let mut generic = Vec::new();
        let mut exact_version = false;
        let mut unresolved_location = false;
        let mut geographic_refusal = false;
        let generic_location_admitted = self
            .source_policy
            .as_ref()
            .is_none_or(|policy| policy.admits_generic_location(&observation.location));
        for index in candidates {
            let factor = &self.records[*index];
            let reason = if factor.flow_version == observation.flow_version {
                exact_version = true;
                if Some(factor.direction) != observation.direction {
                    "direction_mismatch"
                } else if factor.location.is_exact_token()
                    && factor.location == observation.location
                    || self.is_location_independent() && generic_location_admitted
                {
                    exact.push(factor);
                    continue;
                } else if !generic_location_admitted
                    && (self.is_location_independent()
                        || self
                            .source_policy
                            .as_ref()
                            .is_some_and(|policy| policy.is_generic(factor.record_index)))
                {
                    geographic_refusal = true;
                    "generic_location_not_admitted"
                } else if self
                    .source_policy
                    .as_ref()
                    .is_some_and(|policy| policy.is_generic(factor.record_index))
                {
                    generic.push(factor);
                    "reviewed_source_generic_alternative"
                } else if !factor.location.is_exact_token() {
                    unresolved_location = true;
                    "factor_location_semantics_unresolved"
                } else {
                    "location_mismatch"
                }
            } else {
                "flow_version_mismatch"
            };
            decision.rejected.push(RejectedFactor {
                factor: factor.clone(),
                reason: reason.to_owned(),
            });
        }
        let used_generic = exact.is_empty() && !generic.is_empty();
        if used_generic {
            exact = generic;
        }
        match exact.as_slice() {
            [factor] if !factor.unsupported_qualifiers => {
                self.record_selection(&mut decision, factor, used_generic);
            }
            [] => {
                let (status, reason) =
                    unselected_status(exact_version, geographic_refusal, unresolved_location);
                decision.status = status;
                reason.clone_into(&mut decision.reason);
            }
            [factor] => {
                "lcia_factor_qualifier_unsupported".clone_into(&mut decision.reason);
                decision.rejected.push(RejectedFactor {
                    factor: (*factor).clone(),
                    reason: decision.reason.clone(),
                });
            }
            _ => {
                "lcia_factor_applicability_ambiguous".clone_into(&mut decision.reason);
                decision
                    .rejected
                    .extend(exact.into_iter().map(|factor| RejectedFactor {
                        factor: factor.clone(),
                        reason: "multiple_exact_records".to_owned(),
                    }));
            }
        }
        decision
            .rejected
            .sort_by_key(|item| item.factor.record_index);
        decision
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelectionRecord {
    pub impact_index: usize,
    pub flow_index: i32,
    pub decision: FactorDecision,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApplicabilityEvidence {
    pub schema_version: String,
    pub policy_version: String,
    pub sources: Vec<FactorSource>,
    pub observation_groups_sha256: String,
    pub selected: Vec<SelectionRecord>,
    pub unmatched_group_count: u64,
    pub invalid_group_count: u64,
    pub unsupported_direction_group_count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub availability: Option<crate::lcia_availability::LciaAvailability>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unavailable: Vec<SelectionRecord>,
    /// Exact number of unsupported/unmatched method-observation groups evaluated. The detailed
    /// unavailable records are deterministic bounded samples, never a claimed complete expansion.
    #[serde(default)]
    pub unavailable_group_count: u64,
    #[serde(default)]
    pub unavailable_samples_truncated: bool,
}
