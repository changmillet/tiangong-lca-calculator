//! The executable nonregional calculation profile. This chooses source defaults,
//! not geographic/scientific eligibility, coverage or publication policy.
use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde::Deserialize;
use uuid::Uuid;

use crate::calculation_evidence::{RELEASE_BUNDLE_MANIFEST_SHA256, RELEASE_FACTOR_MANIFEST_SHA256};
use crate::static_lcia_cache::{StaticLciaDirection, StaticLciaFactor, VerifiedStaticLciaBundle};

pub const POLICY: &str = "lcia.nonregional-source-defaults.v1";
pub const DEFAULTS: &[u8] = include_bytes!("lcia_nonregional_defaults.json");

#[derive(Deserialize)]
struct PreparedDefaults {
    schema: String,
    static_bundle_manifest_sha256: String,
    static_factor_manifest_sha256: String,
    methods: Vec<PreparedMethod>,
}

#[derive(Deserialize)]
struct PreparedMethod {
    method_id: Uuid,
    method_version: String,
    factor_count: usize,
    default_count: usize,
    alternatives: Vec<PreparedAlternative>,
}

#[derive(Deserialize)]
struct PreparedAlternative {
    flow_id: Uuid,
    flow_version: String,
    direction: String,
    value_bits: String,
    alternative_count: usize,
}

fn defaults() -> anyhow::Result<&'static PreparedDefaults> {
    static PARSED: OnceLock<Result<PreparedDefaults, String>> = OnceLock::new();
    PARSED
        .get_or_init(|| {
            let result = (|| -> anyhow::Result<PreparedDefaults> {
                let value: PreparedDefaults = serde_json::from_slice(DEFAULTS)?;
                anyhow::ensure!(
                    value.schema == "lcia.nonregional-defaults.v1"
                        && value.static_bundle_manifest_sha256 == RELEASE_BUNDLE_MANIFEST_SHA256
                        && value.static_factor_manifest_sha256 == RELEASE_FACTOR_MANIFEST_SHA256,
                    "nonregional defaults source binding mismatch"
                );
                Ok(value)
            })();
            result.map_err(|error| error.to_string())
        })
        .as_ref()
        .map_err(|error| anyhow::anyhow!(error.clone()))
}

/// The numeric cache has no location. Only this prepared, exact-release mapping
/// can restore its defaults. Selection is by exact identity and numeric bits,
/// never array order, equality-based deduplication or regional summation.
pub fn prepare_static_method(
    bundle: &VerifiedStaticLciaBundle,
    method_id: Uuid,
    version: &str,
) -> anyhow::Result<Vec<StaticLciaFactor>> {
    anyhow::ensure!(
        bundle.source_evidence.bundle_manifest_sha256 == RELEASE_BUNDLE_MANIFEST_SHA256
            && bundle.source_evidence.factor_manifest_sha256 == RELEASE_FACTOR_MANIFEST_SHA256,
        "nonregional defaults require the pinned source bundle"
    );
    let method = defaults()?
        .methods
        .iter()
        .find(|method| method.method_id == method_id && method.method_version == version)
        .ok_or_else(|| anyhow::anyhow!("nonregional method defaults missing"))?;
    let records = bundle
        .factors_by_method
        .get(&method_id)
        .ok_or_else(|| anyhow::anyhow!("nonregional factor source missing"))?;
    prepare_records(method, records)
}

fn prepare_records(
    method: &PreparedMethod,
    records: &[StaticLciaFactor],
) -> anyhow::Result<Vec<StaticLciaFactor>> {
    anyhow::ensure!(
        records.len() == method.factor_count,
        "nonregional factor count drift"
    );
    let mut groups = BTreeMap::<_, Vec<_>>::new();
    for record in records {
        groups
            .entry((
                record.flow_id,
                record.flow_version.clone(),
                record.direction,
            ))
            .or_default()
            .push(record);
    }
    anyhow::ensure!(
        groups.len() == method.default_count,
        "nonregional family count drift"
    );
    let mut output = Vec::with_capacity(groups.len());
    for prepared in &method.alternatives {
        let direction = match prepared.direction.as_str() {
            "INPUT" => StaticLciaDirection::Input,
            "OUTPUT" => StaticLciaDirection::Output,
            _ => anyhow::bail!("invalid prepared factor direction"),
        };
        let key = (prepared.flow_id, prepared.flow_version.clone(), direction);
        let records = groups
            .remove(&key)
            .ok_or_else(|| anyhow::anyhow!("nonregional prepared family missing"))?;
        let bits = u64::from_str_radix(&prepared.value_bits, 16)?;
        anyhow::ensure!(
            records.len() == prepared.alternative_count
                && records.iter().any(|record| record.value.to_bits() == bits),
            "nonregional default differs from bound source"
        );
        output.push(StaticLciaFactor {
            flow_id: key.0,
            flow_version: key.1,
            direction,
            value: f64::from_bits(bits),
        });
    }
    for records in groups.into_values() {
        anyhow::ensure!(
            records.len() == 1,
            "regional alternatives lack prepared default"
        );
        output.push(records[0].clone());
    }
    Ok(output)
}

/// Missing/blank native locations are defaults in this named profile. GLO is
/// also an explicit nonregional record; multiple defaults are a malformed model,
/// even if their values happen to be equal. Named country records are not used.
#[must_use]
pub fn is_default_location(value: Option<&serde_json::Value>) -> bool {
    match value {
        None => true,
        Some(serde_json::Value::String(value)) => value.trim().is_empty() || value == "GLO",
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepared_default_replaces_regional_sum_independently_of_order() {
        let method: PreparedMethod = serde_json::from_value(serde_json::json!({
            "method_id":Uuid::nil(), "method_version":"01.00.000", "factor_count":3,
            "default_count":1, "alternatives":[{
                "flow_id":Uuid::nil(), "flow_version":"03.00.004", "direction":"OUTPUT",
                "value_bits":format!("{:016x}",3.02_f64.to_bits()), "alternative_count":3
            }]
        }))
        .unwrap();
        let mut records = [4.0, 11.491, 3.02].map(|value| StaticLciaFactor {
            flow_id: Uuid::nil(),
            flow_version: "03.00.004".into(),
            direction: StaticLciaDirection::Output,
            value,
        });
        let expected = records[2].clone();
        assert_eq!(
            prepare_records(&method, &records).unwrap(),
            vec![expected.clone()]
        );
        records.reverse();
        assert_eq!(prepare_records(&method, &records).unwrap(), vec![expected]);
        records[0].value = 3.0;
        assert!(prepare_records(&method, &records).is_err());
    }

    #[test]
    fn defaults_do_not_depend_on_exchange_or_process_geography() {
        assert!(is_default_location(None));
        assert!(is_default_location(Some(&serde_json::json!(""))));
        assert!(is_default_location(Some(&serde_json::json!("GLO"))));
        for value in [
            serde_json::json!("DE"),
            serde_json::json!("RER"),
            serde_json::json!(null),
        ] {
            assert!(!is_default_location(Some(&value)));
        }
    }
}
