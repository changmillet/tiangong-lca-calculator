//! Worker-owned calculation semantics. Other services only store/compare the digest.
use std::sync::OnceLock;

#[must_use]
pub fn descriptor() -> String {
    serde_json::json!({
        "schema": "lca.calculation-contract.v1",
        "engine": "solver-worker",
        "numerical_policy": crate::NUMERICAL_SNAPSHOT_POLICY_VERSION,
        "allocation_semantics": crate::tidas_process_semantics::TIDAS_ALLOCATION_SEMANTICS_VERSION,
        "source_fingerprint_schema": crate::NUMERICAL_SOURCE_FINGERPRINT_SCHEMA,
        "lcia_selection": crate::lcia_nonregional::POLICY,
        "lcia_defaults_sha256": crate::calculation_evidence::sha256_bytes(crate::lcia_nonregional::DEFAULTS),
        "solver_semantics": "sparse-signed-flow-balance.v1"
    }).to_string()
}

#[must_use]
pub fn fingerprint() -> &'static str {
    static FINGERPRINT: OnceLock<String> = OnceLock::new();
    FINGERPRINT.get_or_init(|| crate::calculation_evidence::sha256_bytes(descriptor().as_bytes()))
}

pub fn ensure_current(recorded: Option<&str>) -> anyhow::Result<()> {
    anyhow::ensure!(
        recorded == Some(fingerprint()),
        "calculation_contract_mismatch: rebuild with the requested calculation implementation"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn descriptor_is_stable_and_bound_to_executable_defaults() {
        let descriptor = super::descriptor();
        assert_eq!(
            super::fingerprint(),
            crate::calculation_evidence::sha256_bytes(descriptor.as_bytes())
        );
        let value: serde_json::Value = serde_json::from_str(&descriptor).unwrap();
        assert_eq!(value["lcia_selection"], crate::lcia_nonregional::POLICY);
        assert_eq!(
            value["lcia_defaults_sha256"],
            crate::calculation_evidence::sha256_bytes(crate::lcia_nonregional::DEFAULTS)
        );
        super::ensure_current(Some(super::fingerprint())).unwrap();
        assert!(super::ensure_current(None).is_err());
        assert!(super::ensure_current(Some("other-implementation")).is_err());
    }
}
