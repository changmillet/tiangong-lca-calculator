//! Frozen category availability shared by snapshots and public result projections.
use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use solver_core::{SolveBatchResult, SolveResult};
use uuid::Uuid;

use crate::snapshot_index::SnapshotImpactMapEntry;

pub const AVAILABILITY_SCHEMA_VERSION: &str = "lcia.category-availability.v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LciaCompleteness {
    Complete,
    Partial,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CategoryStatus {
    Available,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CategoryAvailability {
    pub impact_index: usize,
    pub method_id: Uuid,
    pub method_version: String,
    pub status: CategoryStatus,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LciaAvailability {
    pub schema_version: String,
    pub policy_version: String,
    pub completeness: LciaCompleteness,
    pub categories: Vec<CategoryAvailability>,
}

impl LciaAvailability {
    pub fn new(
        policy_version: String,
        categories: Vec<CategoryAvailability>,
    ) -> anyhow::Result<Self> {
        let available = categories
            .iter()
            .filter(|c| c.status == CategoryStatus::Available)
            .count();
        let completeness = if available == categories.len() && !categories.is_empty() {
            LciaCompleteness::Complete
        } else if available == 0 {
            LciaCompleteness::Unavailable
        } else {
            LciaCompleteness::Partial
        };
        let value = Self {
            schema_version: AVAILABILITY_SCHEMA_VERSION.to_owned(),
            policy_version,
            completeness,
            categories,
        };
        value.validate()?;
        Ok(value)
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.schema_version == AVAILABILITY_SCHEMA_VERSION
                && self.policy_version == crate::lcia_applicability::POLICY_VERSION,
            "lcia_availability_contract_invalid"
        );
        anyhow::ensure!(!self.categories.is_empty(), "lcia_availability_axis_empty");
        anyhow::ensure!(self.categories.len() <= 256, "lcia_availability_axis_limit");
        let mut identities = BTreeSet::new();
        for (index, category) in self.categories.iter().enumerate() {
            anyhow::ensure!(
                category.impact_index == index
                    && identities.insert((category.method_id, category.method_version.clone())),
                "lcia_availability_axis_invalid"
            );
            crate::calculation_bundle::validate_version(
                &category.method_version,
                "availability.method_version",
            )?;
            anyhow::ensure!(
                category.reasons.len() <= 32
                    && category.reasons.windows(2).all(|pair| pair[0] < pair[1]),
                "lcia_availability_reasons_invalid"
            );
            anyhow::ensure!(
                category.reasons.iter().all(|reason| !reason.is_empty()
                    && reason.len() <= 160
                    && reason
                        .bytes()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')),
                "lcia_availability_reasons_invalid"
            );
            anyhow::ensure!(
                (category.status == CategoryStatus::Available) == category.reasons.is_empty(),
                "lcia_availability_reason_status_mismatch"
            );
        }
        let count = self
            .categories
            .iter()
            .filter(|c| c.status == CategoryStatus::Available)
            .count();
        let expected = if count == self.categories.len() {
            LciaCompleteness::Complete
        } else if count == 0 {
            LciaCompleteness::Unavailable
        } else {
            LciaCompleteness::Partial
        };
        anyhow::ensure!(
            self.completeness == expected,
            "lcia_availability_completeness_mismatch"
        );
        Ok(())
    }

    pub fn validate_axis(&self, axis: &[SnapshotImpactMapEntry]) -> anyhow::Result<()> {
        self.validate()?;
        anyhow::ensure!(
            axis.len() == self.categories.len(),
            "lcia_availability_axis_mismatch"
        );
        for (entry, category) in axis.iter().zip(&self.categories) {
            anyhow::ensure!(
                usize::try_from(entry.impact_index)? == category.impact_index
                    && entry.impact_id == category.method_id
                    && entry.impact_version.as_deref() == Some(category.method_version.as_str()),
                "lcia_availability_axis_mismatch"
            );
        }
        Ok(())
    }

    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.completeness == LciaCompleteness::Complete
    }

    pub fn require_complete(&self) -> anyhow::Result<()> {
        self.validate()?;
        anyhow::ensure!(self.is_complete(), "lcia_complete_result_required");
        Ok(())
    }

    pub fn category(&self, index: usize) -> anyhow::Result<&CategoryAvailability> {
        self.categories
            .get(index)
            .filter(|c| c.impact_index == index)
            .ok_or_else(|| anyhow::anyhow!("lcia_availability_category_missing"))
    }

    pub fn require_category_available(&self, index: usize) -> anyhow::Result<()> {
        self.validate()?;
        anyhow::ensure!(
            self.category(index)?.status == CategoryStatus::Available,
            "lcia_category_unavailable"
        );
        Ok(())
    }

    pub fn project_solve_batch_result(&self, result: &SolveBatchResult) -> anyhow::Result<Value> {
        let items = result
            .items
            .iter()
            .map(|item| self.project_solve_result(item))
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok(serde_json::json!({"items":items,"lcia_availability":self}))
    }

    /// Internal C rows can be empty for unavailable categories; they never authorize a public zero.
    pub fn project_solve_result(&self, result: &SolveResult) -> anyhow::Result<Value> {
        self.validate()?;
        let mut value = serde_json::to_value(result)?;
        if let Some(h) = &result.h {
            anyhow::ensure!(
                h.len() == self.categories.len(),
                "lcia_result_availability_axis_mismatch"
            );
            anyhow::ensure!(
                h.iter()
                    .zip(&self.categories)
                    .all(
                        |(amount, category)| category.status == CategoryStatus::Unavailable
                            || amount.is_finite()
                    ),
                "lcia_result_nonfinite"
            );
            value["h"] = Value::Array(
                h.iter()
                    .zip(&self.categories)
                    .map(|(amount, category)| {
                        if category.status == CategoryStatus::Available {
                            serde_json::json!(amount)
                        } else {
                            Value::Null
                        }
                    })
                    .collect(),
            );
        }
        value["lcia_availability"] = serde_json::to_value(self)?;
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use solver_core::FactorizationState;

    fn category(index: usize, available: bool) -> CategoryAvailability {
        CategoryAvailability {
            impact_index: index,
            method_id: Uuid::from_u128(index as u128 + 1),
            method_version: "01.00.000".to_owned(),
            status: if available {
                CategoryStatus::Available
            } else {
                CategoryStatus::Unavailable
            },
            reasons: if available {
                vec![]
            } else {
                vec!["lcia_factor_location_semantics_unresolved".to_owned()]
            },
        }
    }

    #[test]
    fn partial_projection_preserves_inventory_and_known_zero_but_masks_unknown() {
        let availability = LciaAvailability::new(
            crate::lcia_applicability::POLICY_VERSION.to_owned(),
            vec![category(0, true), category(1, false)],
        )
        .unwrap();
        let projected = availability
            .project_solve_result(&SolveResult {
                x: Some(vec![1.0]),
                g: Some(vec![7.0]),
                h: Some(vec![0.0, 0.0]),
                factorization_state: FactorizationState::Ready,
            })
            .unwrap();
        assert_eq!(projected["h"], serde_json::json!([0.0, null]));
        assert_eq!(projected["g"], serde_json::json!([7.0]));
        assert_eq!(projected["lcia_availability"]["completeness"], "partial");
        assert!(availability.require_complete().is_err());
    }

    #[test]
    fn all_unavailable_and_inconsistent_receipts_are_explicit() {
        let mut availability = LciaAvailability::new(
            crate::lcia_applicability::POLICY_VERSION.to_owned(),
            vec![category(0, false)],
        )
        .unwrap();
        assert_eq!(availability.completeness, LciaCompleteness::Unavailable);
        availability.completeness = LciaCompleteness::Complete;
        assert!(availability.validate().is_err());
        assert!(
            LciaAvailability::new(
                crate::lcia_applicability::POLICY_VERSION.to_owned(),
                vec![category(1, true)]
            )
            .is_err()
        );
    }
}
