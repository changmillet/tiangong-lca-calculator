//! Shared TIDAS process calculation semantics.

use std::collections::HashSet;
use std::hash::BuildHasher;

use anyhow::{Context, bail};
use serde_json::Value;

/// Versioned allocation semantics applied before signed-flow linking.
pub const TIDAS_ALLOCATION_SEMANTICS_VERSION: &str = "tidas-reference-allocation-v5";

/// Versioned signed-flow linking semantics applied after allocation.
pub const SIGNED_FLOW_LINK_SEMANTICS_VERSION: &str = "signed-flow-balance-v1";

/// Backward-compatible name for the active allocation semantics identity.
pub const TIDAS_PROCESS_SEMANTICS_VERSION: &str = TIDAS_ALLOCATION_SEMANTICS_VERSION;

// TIDAS `Perc` permits at most three decimal places. Allow a one-unit difference
// in the least-significant percentage digit when checking a closed allocation
// vector (for example, 33.333 + 33.333 + 33.333 = 99.999).
const ALLOCATION_SUM_TOLERANCE: f64 = 0.000_010_000_001;

/// Allocation selected for one process quantitative reference.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TidasAllocationResolution {
    /// The exchange has no `allocations` container.
    Undeclared,
    /// A legacy scalar `allocation: {}` placeholder is treated as undeclared.
    LegacyEmptyUndeclared,
    /// The allocation vector explicitly contains the quantitative reference.
    Explicit { fraction: f64 },
    /// A single targetless full allocation was safely inferred for the unique reference pivot.
    LegacyInferredReference { fraction: f64 },
    /// The closed sparse vector omits the quantitative reference, implying zero.
    SparseZero,
}

/// Process-wide interpretation, before reference normalization and flow linking.
#[derive(Debug)]
pub struct TidasProcessAllocation {
    /// One resolution per source exchange, in the original order.
    pub exchanges: Vec<TidasAllocationResolution>,
    /// Explicit Product/Waste targets whose exact Flow revisions must be verified by the caller.
    pub allocation_target_indices: Vec<usize>,
}

/// Resolve legacy output shares as one process-wide vector, never as independent
/// exchange multipliers. Canonical vectors remain exchange-specific. This does
/// not modify source documents or synthesize new Process identities.
pub fn resolve_tidas_process_allocations(
    exchanges: &[&Value],
    reference_internal_id: &str,
) -> anyhow::Result<TidasProcessAllocation> {
    let reference_internal_id = reference_internal_id.trim();
    let ids = exchanges
        .iter()
        .map(|exchange| match exchange.get("@dataSetInternalID") {
            Some(Value::String(id)) => Some(id.trim().to_owned()),
            Some(Value::Number(id)) => Some(id.to_string()),
            _ => None,
        })
        .collect::<Vec<_>>();
    let mut all_ids = HashSet::new();
    for id in ids.iter().flatten() {
        if id.is_empty() || !all_ids.insert(id.clone()) {
            bail!("invalid or duplicate exchange internal ID={id}");
        }
    }
    let reference_count = ids
        .iter()
        .filter(|id| id.as_deref() == Some(reference_internal_id))
        .count();
    if reference_internal_id.is_empty() || reference_count != 1 {
        bail!("quantitative reference must identify exactly one Process exchange");
    }
    let ProcessAllocationDeclarations { targets, legacy } =
        collect_process_allocation_declarations(exchanges)?;
    if !targets.is_empty() && !legacy.is_empty() {
        bail!("targeted allocations and legacy output shares must not be mixed in one Process");
    }
    let is_output = |index: usize| {
        exchanges[index]
            .get("exchangeDirection")
            .and_then(Value::as_str)
            .is_some_and(|direction| direction.trim() == "Output")
    };
    if let Some(target) = targets
        .iter()
        .filter(|target| !all_ids.contains(*target))
        .min()
    {
        bail!("allocation target {target} must identify an existing unique exchange");
    }
    for (index, id) in ids.iter().enumerate() {
        if id.as_ref().is_some_and(|id| targets.contains(id))
            && !exchanges[index]
                .get("exchangeDirection")
                .and_then(Value::as_str)
                .is_some_and(|direction| matches!(direction.trim(), "Input" | "Output"))
        {
            bail!(
                "allocation target {} requires Input or Output exchangeDirection",
                id.as_ref().unwrap()
            );
        }
    }
    let allocation_target_indices = ids
        .iter()
        .enumerate()
        .filter(|(_, id)| id.as_ref().is_some_and(|id| targets.contains(id)))
        .map(|(index, _)| index)
        .collect();
    // Preserve the bounded historical full-allocation fallback on Input/treatment
    // exchanges only when no legacy output vector or targeted declaration exists.
    if legacy.iter().any(|(index, _)| is_output(*index)) {
        if legacy.iter().any(|(index, _)| !is_output(*index)) {
            bail!("legacy product shares must be declared on Output exchanges only");
        }
        if legacy.iter().any(|(index, _)| ids[*index].is_none()) {
            bail!("legacy output share is missing its exchange internal ID");
        }
        let sum: f64 = legacy.iter().map(|(_, fraction)| fraction).sum();
        if (sum - 1.0).abs() > ALLOCATION_SUM_TOLERANCE {
            bail!(
                "legacy output shares must sum to 100%; actual sum is {}%",
                sum * 100.0
            );
        }
        // Matches the Model reference-default rule when the reference output has
        // no share. Other product views are materialized upstream, not inferred here.
        let fraction = legacy
            .iter()
            .find(|(index, _)| ids[*index].as_deref() == Some(reference_internal_id))
            .map_or(1.0, |(_, fraction)| *fraction);
        return Ok(TidasProcessAllocation {
            exchanges: vec![TidasAllocationResolution::Explicit { fraction }; exchanges.len()],
            allocation_target_indices,
        });
    }
    let resolutions = exchanges
        .iter()
        .map(|exchange| {
            resolve_tidas_exchange_allocation(
                exchange,
                reference_internal_id,
                &all_ids,
                reference_count,
            )
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    Ok(TidasProcessAllocation {
        exchanges: resolutions,
        allocation_target_indices,
    })
}

struct ProcessAllocationDeclarations {
    targets: HashSet<String>,
    legacy: Vec<(usize, f64)>,
}

fn collect_process_allocation_declarations(
    exchanges: &[&Value],
) -> anyhow::Result<ProcessAllocationDeclarations> {
    let mut targets = HashSet::new();
    let mut legacy = Vec::new();
    for (index, exchange) in exchanges.iter().enumerate() {
        let Some(container) = exchange.get("allocations") else {
            continue;
        };
        let allocation = container
            .as_object()
            .context("allocations must be an object")?
            .get("allocation")
            .context("allocations.allocation is missing")?;
        if allocation
            .as_object()
            .is_some_and(serde_json::Map::is_empty)
        {
            continue;
        }
        let entries = match allocation {
            Value::Object(_) => vec![allocation],
            Value::Array(entries) if !entries.is_empty() => entries.iter().collect(),
            _ => bail!("allocations.allocation must be a non-empty object or array"),
        };
        for entry in &entries {
            let entry = entry
                .as_object()
                .context("allocation entry must be an object")?;
            if let Some(target) = entry.get("@internalReferenceToCoProduct") {
                let target = target
                    .as_str()
                    .map(str::trim)
                    .filter(|id| !id.is_empty())
                    .context("allocation target must be a non-empty internal ID")?;
                targets.insert(target.to_owned());
            } else {
                if entries.len() != 1 {
                    bail!("multiple-entry targetless allocation is ambiguous");
                }
                let value = entry
                    .get("@allocatedFraction")
                    .context("targetless allocation fraction is missing")?;
                let canonical = value.as_str().map(|text| {
                    Value::String(
                        text.trim()
                            .strip_suffix('%')
                            .unwrap_or(text.trim())
                            .trim()
                            .to_owned(),
                    )
                });
                legacy.push((
                    index,
                    parse_tidas_perc(canonical.as_ref().unwrap_or(value))?,
                ));
            }
        }
    }
    Ok(ProcessAllocationDeclarations { targets, legacy })
}

/// Resolves one exchange allocation for the process quantitative reference.
///
/// TIDAS stores `@allocatedFraction` as `Perc`, so both JSON strings and numbers
/// are interpreted as percentages and divided by 100. A declared allocation is
/// accepted only when its object/array is non-empty, every target is a unique
/// known exchange, every fraction is finite and within 0..=100, and the complete
/// vector sums to 100% within the three-decimal `Perc` tolerance. Two bounded
/// legacy shapes are normalized: a scalar empty object is undeclared, and one
/// targetless full allocation is attributed to the quantitative reference only
/// when the reference internal ID uniquely resolves to one Process exchange.
pub fn resolve_tidas_exchange_allocation<S: BuildHasher>(
    exchange: &Value,
    reference_internal_id: &str,
    valid_exchange_internal_ids: &HashSet<String, S>,
    reference_exchange_count: usize,
) -> anyhow::Result<TidasAllocationResolution> {
    let reference_internal_id = reference_internal_id.trim();
    if reference_internal_id.is_empty() {
        bail!("quantitative reference internal ID is empty");
    }

    let Some(allocations) = exchange.get("allocations") else {
        return Ok(TidasAllocationResolution::Undeclared);
    };
    let allocations = allocations
        .as_object()
        .context("allocations must be an object")?;
    let allocation = allocations
        .get("allocation")
        .context("allocations.allocation is missing")?;

    if allocation
        .as_object()
        .is_some_and(serde_json::Map::is_empty)
    {
        return Ok(TidasAllocationResolution::LegacyEmptyUndeclared);
    }

    let entries = match allocation {
        Value::Object(_) => vec![allocation],
        Value::Array(entries) if !entries.is_empty() => entries.iter().collect(),
        Value::Array(_) => bail!("allocations.allocation must not be an empty array"),
        _ => bail!("allocations.allocation must be an object or array"),
    };

    if entries.len() == 1 {
        let entry = entries[0]
            .as_object()
            .context("allocations.allocation[0] must be an object")?;
        if !entry.contains_key("@internalReferenceToCoProduct") {
            if reference_exchange_count != 1
                || !valid_exchange_internal_ids.contains(reference_internal_id)
            {
                bail!(
                    "targetless allocation can only be inferred when the quantitative reference uniquely identifies one Process exchange"
                );
            }
            let raw_fraction = entry.get("@allocatedFraction").context(
                "allocations.allocation[0].@allocatedFraction is missing for targetless allocation",
            )?;
            let fraction = parse_legacy_targetless_full_fraction(raw_fraction).context(
                "invalid allocations.allocation[0].@allocatedFraction for targetless allocation",
            )?;
            return Ok(TidasAllocationResolution::LegacyInferredReference { fraction });
        }
    }

    let mut seen_targets = HashSet::with_capacity(entries.len());
    let mut selected_fraction = None;
    let mut fraction_sum = 0.0;

    for (index, entry) in entries.into_iter().enumerate() {
        let entry = entry
            .as_object()
            .with_context(|| format!("allocations.allocation[{index}] must be an object"))?;
        let target = entry
            .get("@internalReferenceToCoProduct")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|target| !target.is_empty())
            .with_context(|| {
                format!(
                    "allocations.allocation[{index}].@internalReferenceToCoProduct is missing or invalid"
                )
            })?;

        if !valid_exchange_internal_ids.contains(target) {
            bail!(
                "allocations.allocation[{index}] references unknown exchange internal ID {target}"
            );
        }
        if !seen_targets.insert(target.to_owned()) {
            bail!("duplicate allocation target internal ID {target}");
        }

        let raw_fraction = entry.get("@allocatedFraction").with_context(|| {
            format!("allocations.allocation[{index}].@allocatedFraction is missing")
        })?;
        let fraction = parse_tidas_perc(raw_fraction).with_context(|| {
            format!("invalid allocations.allocation[{index}].@allocatedFraction")
        })?;
        fraction_sum += fraction;
        if !fraction_sum.is_finite() {
            bail!("allocation fraction sum is non-finite");
        }

        if target == reference_internal_id {
            selected_fraction = Some(fraction);
        }
    }

    if (fraction_sum - 1.0).abs() > ALLOCATION_SUM_TOLERANCE {
        bail!(
            "allocation fractions must sum to 100%; actual sum is {}%",
            fraction_sum * 100.0
        );
    }

    Ok(
        selected_fraction.map_or(TidasAllocationResolution::SparseZero, |fraction| {
            TidasAllocationResolution::Explicit { fraction }
        }),
    )
}

/// Returns the exchange amount value used for calculation, in TIDAS precedence.
#[must_use]
pub fn preferred_calculation_amount_value(exchange: &Value) -> Option<&Value> {
    exchange
        .get("resultingAmount")
        .or_else(|| exchange.get("meanAmount"))
        .or_else(|| exchange.get("meanValue"))
}

fn parse_tidas_perc(value: &Value) -> anyhow::Result<f64> {
    let percentage = match value {
        Value::String(text) => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                bail!("percentage is empty");
            }
            if trimmed.contains('%') {
                bail!("percentage must not include a percent sign");
            }
            trimmed
                .parse::<f64>()
                .context("percentage string is not numeric")?
        }
        Value::Number(number) => number
            .as_f64()
            .context("percentage number cannot be represented as f64")?,
        _ => bail!("percentage must be a string or number"),
    };

    if !percentage.is_finite() {
        bail!("percentage is non-finite");
    }
    if !(0.0..=100.0).contains(&percentage) {
        bail!("percentage must be within 0..=100");
    }
    Ok(percentage / 100.0)
}

fn parse_legacy_targetless_full_fraction(value: &Value) -> anyhow::Result<f64> {
    if value.as_str().is_some_and(|text| text.trim() == "100%") {
        return Ok(1.0);
    }

    let fraction = parse_tidas_perc(value)?;
    if fraction.to_bits() != 1.0_f64.to_bits() {
        bail!("targetless allocation fraction must be exactly 100%");
    }
    Ok(1.0)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use serde_json::{Value, json};

    use super::{
        SIGNED_FLOW_LINK_SEMANTICS_VERSION, TIDAS_ALLOCATION_SEMANTICS_VERSION,
        TIDAS_PROCESS_SEMANTICS_VERSION, TidasAllocationResolution,
        preferred_calculation_amount_value,
        resolve_tidas_exchange_allocation as resolve_tidas_exchange_allocation_with_count,
    };

    fn outputs(ids: &[&str]) -> HashSet<String> {
        ids.iter().map(|id| (*id).to_owned()).collect()
    }

    fn resolve_tidas_exchange_allocation(
        exchange: &Value,
        reference_internal_id: &str,
        valid_exchange_internal_ids: &HashSet<String>,
    ) -> anyhow::Result<TidasAllocationResolution> {
        resolve_tidas_exchange_allocation_with_count(
            exchange,
            reference_internal_id,
            valid_exchange_internal_ids,
            usize::from(valid_exchange_internal_ids.contains(reference_internal_id)),
        )
    }

    fn assert_fraction(resolution: TidasAllocationResolution, expected: f64) {
        let TidasAllocationResolution::Explicit { fraction } = resolution else {
            panic!("expected explicit allocation, got {resolution:?}");
        };
        assert!((fraction - expected).abs() <= f64::EPSILON);
    }

    fn process_allocations(
        rows: &Value,
        reference: &str,
    ) -> anyhow::Result<super::TidasProcessAllocation> {
        super::resolve_tidas_process_allocations(
            &rows.as_array().unwrap().iter().collect::<Vec<_>>(),
            reference,
        )
    }

    fn legacy_process() -> Value {
        json!([
            {"@dataSetInternalID":"a", "exchangeDirection":"Output", "allocations":{"allocation":{"@allocatedFraction":"70%"}}},
            {"@dataSetInternalID":"b", "exchangeDirection":"Output", "allocations":{"allocation":[{"@allocatedFraction":30}]}},
            {"@dataSetInternalID":"input", "exchangeDirection":"Input"},
            {"@dataSetInternalID":"emission", "exchangeDirection":"Output"}
        ])
    }

    #[test]
    fn process_legacy_shares_apply_to_every_exchange_without_mutating_source() {
        let rows = legacy_process();
        let original = rows.clone();
        for (reference, expected) in [("a", 0.7), ("b", 0.3)] {
            let result = process_allocations(&rows, reference).unwrap();
            for resolved in result.exchanges {
                assert_fraction(resolved, expected);
            }
            assert_eq!(result.allocation_target_indices, [] as [usize; 0]);
        }
        assert_eq!(rows, original);
    }

    #[test]
    fn process_legacy_reference_default_zero_and_rounding_are_preserved() {
        let mut rows = legacy_process();
        rows[0]["allocations"]["allocation"]["@allocatedFraction"] = json!(0);
        rows[1]["allocations"]["allocation"][0]["@allocatedFraction"] = json!(100);
        assert_fraction(process_allocations(&rows, "a").unwrap().exchanges[2], 0.0);
        assert_fraction(
            process_allocations(&rows, "emission").unwrap().exchanges[2],
            1.0,
        );
        rows[0]["allocations"]["allocation"]["@allocatedFraction"] = json!(33.333);
        rows[1]["allocations"]["allocation"][0]["@allocatedFraction"] = json!(66.666);
        assert!(process_allocations(&rows, "a").is_ok());
    }

    #[test]
    fn process_legacy_invalid_or_mixed_declarations_fail_closed() {
        for fraction in [
            json!(""),
            json!("NaN"),
            json!("70%%"),
            json!(-1),
            json!(101),
            json!(69),
        ] {
            let mut rows = legacy_process();
            rows[0]["allocations"]["allocation"]["@allocatedFraction"] = fraction;
            assert!(process_allocations(&rows, "a").is_err(), "{rows}");
        }
        let mut rows = legacy_process();
        rows[2]["allocations"] =
            json!({"allocation":{"@internalReferenceToCoProduct":"a", "@allocatedFraction":100}});
        assert!(
            process_allocations(&rows, "a")
                .unwrap_err()
                .to_string()
                .contains("mixed")
        );
        rows[2]["allocations"] = json!({"allocation":{"@allocatedFraction":100}});
        assert!(process_allocations(&rows, "a").is_err());
        let mut rows = legacy_process();
        rows[1]["exchangeDirection"] = json!("Input");
        assert!(process_allocations(&rows, "a").is_err());
    }

    #[test]
    fn process_targeted_vectors_validate_either_direction_targets_and_keep_sparse_defaults() {
        let mut rows = legacy_process();
        rows[0].as_object_mut().unwrap().remove("allocations");
        rows[1].as_object_mut().unwrap().remove("allocations");
        rows[2]["allocations"] =
            json!({"allocation":{"@internalReferenceToCoProduct":"b", "@allocatedFraction":100}});
        let result = process_allocations(&rows, "a").unwrap();
        assert_eq!(result.exchanges[2], TidasAllocationResolution::SparseZero);
        assert_eq!(result.exchanges[3], TidasAllocationResolution::Undeclared);
        assert_eq!(result.allocation_target_indices, vec![1]);
        rows[2]["allocations"]["allocation"]["@internalReferenceToCoProduct"] = json!("input");
        assert_eq!(
            process_allocations(&rows, "input")
                .unwrap()
                .allocation_target_indices,
            vec![2]
        );
        rows[2]["allocations"]["allocation"]["@internalReferenceToCoProduct"] = json!("missing");
        assert!(process_allocations(&rows, "a").is_err());
    }

    #[test]
    fn explicit_targets_reject_missing_duplicate_and_invalid_direction_identities() {
        let rows = json!([
            {"@dataSetInternalID":"ref", "exchangeDirection":"Input"},
            {"@dataSetInternalID":"residual", "exchangeDirection":"Output",
             "allocations":{"allocation":{"@internalReferenceToCoProduct":"ref", "@allocatedFraction":100}}}
        ]);
        assert!(process_allocations(&rows, "ref").is_ok());
        for direction in [json!(null), json!("unknown"), json!("output")] {
            let mut invalid = rows.clone();
            invalid[0]["exchangeDirection"] = direction;
            assert!(
                process_allocations(&invalid, "ref")
                    .unwrap_err()
                    .to_string()
                    .contains("exchangeDirection")
            );
        }
        let mut duplicate = rows.clone();
        duplicate[1]["@dataSetInternalID"] = json!("ref");
        assert!(
            process_allocations(&duplicate, "ref")
                .unwrap_err()
                .to_string()
                .contains("duplicate")
        );
        let mut missing = rows;
        missing[0]
            .as_object_mut()
            .unwrap()
            .remove("@dataSetInternalID");
        assert!(
            process_allocations(&missing, "residual")
                .unwrap_err()
                .to_string()
                .contains("existing unique")
        );
    }

    #[test]
    fn process_input_reference_and_bounded_legacy_full_fallback_remain_valid() {
        let rows = json!([
            {"@dataSetInternalID":"input", "exchangeDirection":"Input"},
            {"@dataSetInternalID":"emission", "exchangeDirection":"Output"}
        ]);
        assert_eq!(
            process_allocations(&rows, "input").unwrap().exchanges[0],
            TidasAllocationResolution::Undeclared
        );
        let mut rows = rows;
        rows[0]["allocations"] = json!({"allocation":{"@allocatedFraction":"100%"}});
        assert!(matches!(
            process_allocations(&rows, "input").unwrap().exchanges[0],
            TidasAllocationResolution::LegacyInferredReference { .. }
        ));
        rows[0]["allocations"]["allocation"]["@allocatedFraction"] = json!(70);
        assert!(process_allocations(&rows, "input").is_err());
    }

    #[test]
    fn semantics_version_is_stable() {
        assert_eq!(
            TIDAS_PROCESS_SEMANTICS_VERSION,
            "tidas-reference-allocation-v5"
        );
        assert_eq!(
            TIDAS_ALLOCATION_SEMANTICS_VERSION,
            "tidas-reference-allocation-v5"
        );
        assert_eq!(SIGNED_FLOW_LINK_SEMANTICS_VERSION, "signed-flow-balance-v1");
    }

    #[test]
    fn missing_allocations_container_is_undeclared() {
        let resolution = resolve_tidas_exchange_allocation(&json!({}), "1", &outputs(&["1"]))
            .expect("resolve undeclared");
        assert_eq!(resolution, TidasAllocationResolution::Undeclared);
    }

    #[test]
    fn scalar_empty_allocation_is_legacy_undeclared() {
        let resolution = resolve_tidas_exchange_allocation(
            &json!({ "allocations": { "allocation": {} } }),
            "1",
            &outputs(&["1"]),
        )
        .expect("resolve legacy empty placeholder");
        assert_eq!(resolution, TidasAllocationResolution::LegacyEmptyUndeclared);
    }

    #[test]
    fn targetless_full_allocation_is_inferred_for_the_unique_reference_exchange() {
        for fraction in [json!("100"), json!("100.000"), json!(100), json!("100%")] {
            let exchange = json!({
                "allocations": {
                    "allocation": { "@allocatedFraction": fraction }
                }
            });
            let resolution = resolve_tidas_exchange_allocation(&exchange, "1", &outputs(&["1"]))
                .expect("infer targetless full allocation");
            let TidasAllocationResolution::LegacyInferredReference { fraction } = resolution else {
                panic!("expected legacy inferred allocation, got {resolution:?}");
            };
            assert!((fraction - 1.0).abs() <= f64::EPSILON);
        }
    }

    #[test]
    fn one_element_targetless_array_is_inferred_when_unambiguous() {
        let exchange = json!({
            "allocations": {
                "allocation": [{ "@allocatedFraction": "100" }]
            }
        });
        let resolution = resolve_tidas_exchange_allocation(&exchange, "1", &outputs(&["1"]))
            .expect("infer one targetless array entry");
        assert_eq!(
            resolution,
            TidasAllocationResolution::LegacyInferredReference { fraction: 1.0 }
        );
    }

    #[test]
    fn targetless_allocation_requires_exactly_one_reference_exchange() {
        let exchange = json!({
            "allocations": {
                "allocation": { "@allocatedFraction": "100" }
            }
        });
        let valid_outputs = outputs(&["1"]);

        for reference_exchange_count in [0, 2] {
            let error = resolve_tidas_exchange_allocation_with_count(
                &exchange,
                "1",
                &valid_outputs,
                reference_exchange_count,
            )
            .expect_err("reject non-unique reference exchange");
            assert!(error.to_string().contains("uniquely identifies"));
        }

        let error = resolve_tidas_exchange_allocation_with_count(&exchange, "2", &valid_outputs, 1)
            .expect_err("reject unique output that is not the reference");
        assert!(error.to_string().contains("quantitative reference"));
    }

    #[test]
    fn targetless_non_full_or_malformed_allocations_are_rejected() {
        for fraction in [
            Some(json!("0")),
            Some(json!("1.5")),
            Some(json!("60")),
            Some(json!("94%")),
            Some(json!("99.999")),
            Some(json!("")),
            None,
        ] {
            let mut entry = serde_json::Map::new();
            if let Some(fraction) = fraction {
                entry.insert("@allocatedFraction".to_owned(), fraction);
            } else {
                entry.insert("legacyNote".to_owned(), json!(true));
            }
            let exchange = json!({
                "allocations": {
                    "allocation": Value::Object(entry)
                }
            });
            assert!(resolve_tidas_exchange_allocation(&exchange, "1", &outputs(&["1"])).is_err());
        }
    }

    #[test]
    fn targetless_reference_inference_does_not_depend_on_other_exchange_directions() {
        let multiple_exchanges = json!({
            "allocations": {
                "allocation": { "@allocatedFraction": "100" }
            }
        });
        assert_eq!(
            resolve_tidas_exchange_allocation(&multiple_exchanges, "1", &outputs(&["1", "2"]))
                .expect("infer unique reference among multiple exchanges"),
            TidasAllocationResolution::LegacyInferredReference { fraction: 1.0 }
        );
    }

    #[test]
    fn multiple_targetless_entries_remain_invalid() {
        let multiple_entries = json!({
            "allocations": {
                "allocation": [
                    { "@allocatedFraction": "60" },
                    { "@allocatedFraction": "40" }
                ]
            }
        });
        assert!(
            resolve_tidas_exchange_allocation(&multiple_entries, "1", &outputs(&["1"])).is_err()
        );
    }

    #[test]
    fn object_allocation_resolves_explicit_reference() {
        let exchange = json!({
            "allocations": {
                "allocation": {
                    "@internalReferenceToCoProduct": "1",
                    "@allocatedFraction": "100.000"
                }
            }
        });
        let resolution = resolve_tidas_exchange_allocation(&exchange, "1", &outputs(&["1"]))
            .expect("resolve object");
        assert_fraction(resolution, 1.0);
    }

    #[test]
    fn string_half_percent_is_divided_by_one_hundred() {
        let exchange = json!({
            "allocations": {
                "allocation": [
                    {
                        "@internalReferenceToCoProduct": "1",
                        "@allocatedFraction": "0.500"
                    },
                    {
                        "@internalReferenceToCoProduct": "2",
                        "@allocatedFraction": "99.500"
                    }
                ]
            }
        });
        let resolution = resolve_tidas_exchange_allocation(&exchange, "1", &outputs(&["1", "2"]))
            .expect("resolve half percent");
        assert_fraction(resolution, 0.005);
    }

    #[test]
    fn numeric_one_percent_is_divided_by_one_hundred() {
        let exchange = json!({
            "allocations": {
                "allocation": [
                    {
                        "@internalReferenceToCoProduct": "1",
                        "@allocatedFraction": 1
                    },
                    {
                        "@internalReferenceToCoProduct": "2",
                        "@allocatedFraction": 99
                    }
                ]
            }
        });
        let resolution = resolve_tidas_exchange_allocation(&exchange, "1", &outputs(&["1", "2"]))
            .expect("resolve one percent");
        assert_fraction(resolution, 0.01);
    }

    #[test]
    fn array_selects_reference_target_independent_of_order() {
        let forward = json!({
            "allocations": {
                "allocation": [
                    {
                        "@internalReferenceToCoProduct": "1",
                        "@allocatedFraction": "60.000"
                    },
                    {
                        "@internalReferenceToCoProduct": "2",
                        "@allocatedFraction": "40.000"
                    }
                ]
            }
        });
        let reversed = json!({
            "allocations": {
                "allocation": [
                    {
                        "@internalReferenceToCoProduct": "2",
                        "@allocatedFraction": "40.000"
                    },
                    {
                        "@internalReferenceToCoProduct": "1",
                        "@allocatedFraction": "60.000"
                    }
                ]
            }
        });
        let valid_outputs = outputs(&["1", "2"]);

        assert_fraction(
            resolve_tidas_exchange_allocation(&forward, "1", &valid_outputs)
                .expect("resolve forward A"),
            0.6,
        );
        assert_fraction(
            resolve_tidas_exchange_allocation(&reversed, "1", &valid_outputs)
                .expect("resolve reversed A"),
            0.6,
        );
        assert_fraction(
            resolve_tidas_exchange_allocation(&forward, "2", &valid_outputs).expect("resolve B"),
            0.4,
        );
    }

    #[test]
    fn closed_vector_without_reference_is_sparse_zero() {
        let exchange = json!({
            "allocations": {
                "allocation": {
                    "@internalReferenceToCoProduct": "2",
                    "@allocatedFraction": "100.000"
                }
            }
        });
        let resolution = resolve_tidas_exchange_allocation(&exchange, "1", &outputs(&["1", "2"]))
            .expect("resolve sparse zero");
        assert_eq!(resolution, TidasAllocationResolution::SparseZero);
    }

    #[test]
    fn explicit_zero_for_reference_is_valid() {
        let exchange = json!({
            "allocations": {
                "allocation": [
                    {
                        "@internalReferenceToCoProduct": "1",
                        "@allocatedFraction": "0"
                    },
                    {
                        "@internalReferenceToCoProduct": "2",
                        "@allocatedFraction": "100"
                    }
                ]
            }
        });
        let resolution = resolve_tidas_exchange_allocation(&exchange, "1", &outputs(&["1", "2"]))
            .expect("resolve explicit zero");
        assert_fraction(resolution, 0.0);
    }

    #[test]
    fn three_decimal_rounding_tolerance_accepts_99_999_percent() {
        let exchange = json!({
            "allocations": {
                "allocation": [
                    {
                        "@internalReferenceToCoProduct": "1",
                        "@allocatedFraction": "33.333"
                    },
                    {
                        "@internalReferenceToCoProduct": "2",
                        "@allocatedFraction": "33.333"
                    },
                    {
                        "@internalReferenceToCoProduct": "3",
                        "@allocatedFraction": "33.333"
                    }
                ]
            }
        });
        let resolution =
            resolve_tidas_exchange_allocation(&exchange, "1", &outputs(&["1", "2", "3"]))
                .expect("accept three-decimal rounding");
        assert_fraction(resolution, 0.333_33);
    }

    #[test]
    fn duplicate_target_is_rejected() {
        let exchange = json!({
            "allocations": {
                "allocation": [
                    {
                        "@internalReferenceToCoProduct": "1",
                        "@allocatedFraction": "60"
                    },
                    {
                        "@internalReferenceToCoProduct": "1",
                        "@allocatedFraction": "40"
                    }
                ]
            }
        });
        let error = resolve_tidas_exchange_allocation(&exchange, "1", &outputs(&["1"]))
            .expect_err("reject duplicate target");
        assert!(error.to_string().contains("duplicate"));
    }

    #[test]
    fn unknown_target_is_rejected() {
        let exchange = json!({
            "allocations": {
                "allocation": {
                    "@internalReferenceToCoProduct": "2",
                    "@allocatedFraction": "100"
                }
            }
        });
        let error = resolve_tidas_exchange_allocation(&exchange, "1", &outputs(&["1"]))
            .expect_err("reject unknown target");
        assert!(error.to_string().contains("unknown exchange"));
    }

    #[test]
    fn empty_array_is_rejected() {
        let exchange = json!({ "allocations": { "allocation": [] } });
        let error = resolve_tidas_exchange_allocation(&exchange, "1", &outputs(&["1"]))
            .expect_err("reject empty array");
        assert!(error.to_string().contains("empty array"));
    }

    #[test]
    fn malformed_allocation_shapes_and_fields_are_rejected() {
        let cases = [
            json!({ "allocations": {} }),
            json!({ "allocations": { "allocation": "100" } }),
            json!({ "allocations": { "allocation": [{}] } }),
            json!({
                "allocations": {
                    "allocation": {
                        "@internalReferenceToCoProduct": null,
                        "@allocatedFraction": "100"
                    }
                }
            }),
            json!({
                "allocations": {
                    "allocation": {
                        "@internalReferenceToCoProduct": "",
                        "@allocatedFraction": "100"
                    }
                }
            }),
            json!({
                "allocations": {
                    "allocation": {
                        "@internalReferenceToCoProduct": "1"
                    }
                }
            }),
        ];

        for exchange in cases {
            assert!(resolve_tidas_exchange_allocation(&exchange, "1", &outputs(&["1"])).is_err());
        }
    }

    #[test]
    fn allocation_sum_mismatch_is_rejected() {
        let exchange = json!({
            "allocations": {
                "allocation": [
                    {
                        "@internalReferenceToCoProduct": "1",
                        "@allocatedFraction": "60"
                    },
                    {
                        "@internalReferenceToCoProduct": "2",
                        "@allocatedFraction": "30"
                    }
                ]
            }
        });
        let error = resolve_tidas_exchange_allocation(&exchange, "1", &outputs(&["1", "2"]))
            .expect_err("reject sum mismatch");
        assert!(error.to_string().contains("sum to 100%"));
    }

    #[test]
    fn percent_sign_is_rejected() {
        let exchange = json!({
            "allocations": {
                "allocation": {
                    "@internalReferenceToCoProduct": "1",
                    "@allocatedFraction": "100%"
                }
            }
        });
        let error = resolve_tidas_exchange_allocation(&exchange, "1", &outputs(&["1"]))
            .expect_err("reject percent sign");
        assert!(format!("{error:#}").contains("percent sign"));
    }

    #[test]
    fn non_finite_and_out_of_range_percentages_are_rejected() {
        for fraction in [
            json!("NaN"),
            json!("Infinity"),
            json!("-0.001"),
            json!("100.001"),
        ] {
            let exchange = json!({
                "allocations": {
                    "allocation": {
                        "@internalReferenceToCoProduct": "1",
                        "@allocatedFraction": fraction
                    }
                }
            });
            assert!(resolve_tidas_exchange_allocation(&exchange, "1", &outputs(&["1"])).is_err());
        }
    }

    #[test]
    fn preferred_amount_uses_resulting_then_mean_then_legacy_mean_value() {
        let all = json!({
            "resultingAmount": "3",
            "meanAmount": "2",
            "meanValue": "1"
        });
        let mean = json!({ "meanAmount": "2", "meanValue": "1" });
        let legacy = json!({ "meanValue": "1" });

        assert_eq!(
            preferred_calculation_amount_value(&all),
            Some(&Value::String("3".to_owned()))
        );
        assert_eq!(
            preferred_calculation_amount_value(&mean),
            Some(&Value::String("2".to_owned()))
        );
        assert_eq!(
            preferred_calculation_amount_value(&legacy),
            Some(&Value::String("1".to_owned()))
        );
        assert_eq!(preferred_calculation_amount_value(&json!({})), None);
    }
}
