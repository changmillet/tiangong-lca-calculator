use super::{
    contracts::{
        Bounds, Connection, DatasetRef, Exchange, Instance, NormalizedModel, Port, SOURCE_SCHEMA,
        SourceIdentity, SourceInput,
    },
    identity::sha256,
};
use anyhow::{Context, bail, ensure};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

type Documents = BTreeMap<(String, String, String), Value>;
fn text(value: &Value) -> anyhow::Result<String> {
    value
        .as_str()
        .map(str::to_owned)
        .or_else(|| value.as_i64().map(|n| n.to_string()))
        .context("study_missing_text")
}
fn items(value: &Value) -> Vec<&Value> {
    value.as_array().map_or_else(
        || if value.is_null() { vec![] } else { vec![value] },
        |v| v.iter().collect(),
    )
}
fn reference(value: &Value) -> anyhow::Result<DatasetRef> {
    Ok(DatasetRef {
        id: text(&value["@refObjectId"])?,
        version: text(&value["@version"])?,
    })
}
fn lookup<'a>(docs: &'a Documents, kind: &str, identity: &DatasetRef) -> anyhow::Result<&'a Value> {
    docs.get(&(
        kind.to_owned(),
        identity.id.clone(),
        identity.version.clone(),
    ))
    .context("study_exact_source_missing")
}
fn safe_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}
fn amount(value: &Value) -> anyhow::Result<f64> {
    let amount = value
        .as_f64()
        .or_else(|| value.as_str().and_then(|s| s.parse().ok()))
        .context("study_invalid_amount")?;
    ensure!(amount.is_finite(), "study_nonfinite_amount");
    Ok(if amount == 0.0 { 0.0 } else { amount })
}
fn exchange_amount(value: &Value) -> anyhow::Result<f64> {
    for key in ["resultingAmount", "meanAmount", "meanValue"] {
        if !value[key].is_null() {
            return amount(&value[key]);
        }
    }
    bail!("study_missing_exchange_amount")
}
#[allow(clippy::float_cmp)] // Exact reference units: tolerance would admit unsupported conversion.
fn unit(docs: &Documents, flow: &DatasetRef) -> anyhow::Result<(String, Vec<DatasetRef>, String)> {
    let data = &lookup(docs, "flow", flow)?["flowDataSet"];
    let pivot = text(
        &data["flowInformation"]["quantitativeReference"]["referenceToReferenceFlowProperty"],
    )?;
    let props = items(&data["flowProperties"]["flowProperty"]);
    let selected = props
        .iter()
        .filter(|p| text(&p["@dataSetInternalID"]).ok().as_ref() == Some(&pivot))
        .collect::<Vec<_>>();
    ensure!(selected.len() == 1, "study_flow_reference_property");
    ensure!(
        amount(&selected[0]["meanValue"])? == 1.0,
        "study_unsupported_unit_conversion"
    );
    let prop = reference(&selected[0]["referenceToFlowPropertyDataSet"])?;
    let property = &lookup(docs, "flowproperty", &prop)?["flowPropertyDataSet"];
    let group = reference(
        &property["flowPropertiesInformation"]["quantitativeReference"]["referenceToReferenceUnitGroup"],
    )?;
    let group_data = &lookup(docs, "unitgroup", &group)?["unitGroupDataSet"];
    let pivot = text(
        &group_data["unitGroupInformation"]["quantitativeReference"]["referenceToReferenceUnit"],
    )?;
    let units = items(&group_data["units"]["unit"]);
    let selected = units
        .iter()
        .filter(|u| text(&u["@dataSetInternalID"]).ok().as_ref() == Some(&pivot))
        .collect::<Vec<_>>();
    ensure!(
        selected.len() == 1 && amount(&selected[0]["meanValue"])? == 1.0,
        "study_reference_unit"
    );
    Ok((
        text(&selected[0]["name"])?,
        vec![prop, group],
        text(&data["modellingAndValidation"]["LCIMethod"]["typeOfDataSet"])?,
    ))
}
fn port_for_flow(
    instance: &Instance,
    flow_id: &str,
    version: &str,
    direction: &str,
) -> anyhow::Result<Port> {
    let matches = instance
        .exchanges
        .iter()
        .filter(|e| e.flow.id == flow_id && e.flow.version == version && e.direction == direction)
        .collect::<Vec<_>>();
    ensure!(
        matches.len() == 1,
        "study_ambiguous_or_missing_exchange_port"
    );
    Ok(Port {
        instance_id: instance.id.clone(),
        exchange_id: matches[0].id.clone(),
    })
}
#[allow(clippy::too_many_lines)]
pub(super) fn normalize(
    source: &SourceInput,
    bounds: &Bounds,
    base_owned_bytes: usize,
) -> anyhow::Result<(NormalizedModel, usize)> {
    ensure!(
        source.schema_version == SOURCE_SCHEMA,
        "study_source_schema"
    );
    ensure!(
        source.documents.len() <= bounds.max_documents,
        "study_document_limit"
    );
    let mut docs = Documents::new();
    let mut identities = Vec::new();
    for doc in &source.documents {
        ensure!(
            doc.state_code == 100
                || (doc.state_code == 0
                    && source.actor_id.as_ref().is_some_and(
                        |actor| !actor.is_empty() && doc.owner_actor_id.as_ref() == Some(actor)
                    )),
            "study_numerical_source_not_eligible"
        );
        ensure!(
            sha256(doc.json.as_bytes()) == doc.sha256,
            "study_source_document_hash"
        );
        let value: Value = super::json::from_str(&doc.json)?;
        let (root, info) = match doc.kind.as_str() {
            "lifecyclemodel" => ("lifeCycleModelDataSet", "lifeCycleModelInformation"),
            "process" => ("processDataSet", "processInformation"),
            "flow" => ("flowDataSet", "flowInformation"),
            "flowproperty" => ("flowPropertyDataSet", "flowPropertiesInformation"),
            "unitgroup" => ("unitGroupDataSet", "unitGroupInformation"),
            _ => bail!("study_source_kind"),
        };
        ensure!(
            text(&value[root][info]["dataSetInformation"]["common:UUID"])? == doc.identity.id
                && text(
                    &value[root]["administrativeInformation"]["publicationAndOwnership"]["common:dataSetVersion"]
                )? == doc.identity.version,
            "study_source_identity_mismatch"
        );
        ensure!(
            docs.insert(
                (
                    doc.kind.clone(),
                    doc.identity.id.clone(),
                    doc.identity.version.clone()
                ),
                value
            )
            .is_none(),
            "study_duplicate_source"
        );
        identities.push(SourceIdentity {
            kind: doc.kind.clone(),
            identity: doc.identity.clone(),
            state_code: doc.state_code,
            sha256: doc.sha256.clone(),
        });
    }
    let model = lookup(&docs, "lifecyclemodel", &source.origin)?;
    let data = &model["lifeCycleModelDataSet"]["lifeCycleModelInformation"];
    let raw_instances = items(&data["technology"]["processes"]["processInstance"]);
    ensure!(
        !raw_instances.is_empty() && raw_instances.len() <= bounds.max_instances,
        "study_instance_limit"
    );
    // Preflight every expansion while documents are borrowed, before copying any per-instance
    // allocation metadata. The estimate covers normalized AST clones and hash/JSON staging.
    let mut expanded_bytes = 0usize;
    for raw in &raw_instances {
        let process = reference(&raw["referenceToProcess"])?;
        let size = encoded_size(lookup(&docs, "process", &process)?)?;
        expanded_bytes = expanded_bytes
            .checked_add(size)
            .context("study_source_expansion_overflow")?;
        ensure!(
            expanded_bytes <= bounds.max_artifact_bytes,
            "study_source_expansion_artifact_limit"
        );
        ensure!(
            base_owned_bytes.saturating_add(expanded_bytes.saturating_mul(32))
                <= bounds.max_owned_bytes,
            "study_source_expansion_memory_limit"
        );
    }
    let mut instances = Vec::new();
    let mut ids = BTreeSet::new();
    let mut count = 0;
    for raw in &raw_instances {
        let id = text(&raw["@dataSetInternalID"])?;
        ensure!(
            safe_id(&id) && ids.insert(id.clone()),
            "study_instance_identity"
        );
        let process = reference(&raw["referenceToProcess"])?;
        let data = &lookup(&docs, "process", &process)?["processDataSet"];
        let reference_exchange_id =
            text(&data["processInformation"]["quantitativeReference"]["referenceToReferenceFlow"])?;
        let mut exchanges = Vec::new();
        let mut ids = BTreeSet::new();
        for raw in items(&data["exchanges"]["exchange"]) {
            count += 1;
            ensure!(count <= bounds.max_exchanges, "study_exchange_limit");
            let id = text(&raw["@dataSetInternalID"])?;
            ensure!(
                safe_id(&id) && ids.insert(id.clone()),
                "study_exchange_identity"
            );
            let direction = text(&raw["exchangeDirection"])?;
            ensure!(
                matches!(direction.as_str(), "Input" | "Output"),
                "study_exchange_direction"
            );
            let flow = reference(&raw["referenceToFlowDataSet"])?;
            let (unit, unit_identity, kind) = unit(&docs, &flow)?;
            ensure!(
                matches!(
                    kind.as_str(),
                    "Product flow" | "Waste flow" | "Elementary flow"
                ),
                "study_unsupported_flow_kind"
            );
            exchanges.push(Exchange {
                id,
                flow,
                direction,
                amount: exchange_amount(raw)?,
                kind,
                unit,
                unit_identity,
                allocations: raw.get("allocations").cloned(),
            });
        }
        ensure!(
            ids.contains(&reference_exchange_id),
            "study_reference_exchange_missing"
        );
        exchanges.sort_by(|a, b| a.id.cmp(&b.id));
        instances.push(Instance {
            id,
            process,
            reference_exchange_id,
            exchanges,
        });
    }
    let ref_id = text(&data["quantitativeReference"]["referenceToReferenceProcess"])?;
    let ref_instance = instances
        .iter()
        .find(|i| i.id == ref_id)
        .context("study_reference_instance")?;
    let reference = Port {
        instance_id: ref_id,
        exchange_id: ref_instance.reference_exchange_id.clone(),
    };
    let mut connections = Vec::new();
    let mut consumers = BTreeSet::new();
    for raw in raw_instances {
        let id = text(&raw["@dataSetInternalID"])?;
        let supplier = instances.iter().find(|i| i.id == id).unwrap();
        for output in items(&raw["connections"]["outputExchange"]) {
            let flow = text(&output["@flowUUID"])?;
            let version = text(&output["@version"])?;
            // Exact ports are required; no UUID-only or version fallback.
            let supplier_port = if let Some(id) = output.get("@exchangeId") {
                Port {
                    instance_id: id_for(supplier),
                    exchange_id: text(id)?,
                }
            } else {
                port_for_flow(supplier, &flow, &version, "Output")?
            };
            let declared_supplier = supplier
                .exchanges
                .iter()
                .find(|e| e.id == supplier_port.exchange_id)
                .context("study_supplier_exchange")?;
            ensure!(
                declared_supplier.flow.id == flow && declared_supplier.flow.version == version,
                "study_declared_connection_flow_mismatch"
            );
            for downstream in items(&output["downstreamProcess"]) {
                let id = text(&downstream["@id"])?;
                let consumer = instances
                    .iter()
                    .find(|i| i.id == id)
                    .context("study_connection_instance")?;
                let consumer_port = if let Some(id) = downstream.get("@exchangeId") {
                    Port {
                        instance_id: consumer.id.clone(),
                        exchange_id: text(id)?,
                    }
                } else {
                    port_for_flow(
                        consumer,
                        &text(&downstream["@flowUUID"])?,
                        &text(&downstream["@version"])?,
                        "Input",
                    )?
                };
                let declared_consumer = consumer
                    .exchanges
                    .iter()
                    .find(|e| e.id == consumer_port.exchange_id)
                    .context("study_consumer_exchange")?;
                ensure!(
                    declared_consumer.flow.id == text(&downstream["@flowUUID"])?
                        && declared_consumer.flow.version == text(&downstream["@version"])?,
                    "study_declared_connection_flow_mismatch"
                );
                ensure!(
                    consumers.insert((
                        consumer_port.instance_id.clone(),
                        consumer_port.exchange_id.clone()
                    )),
                    "study_multiple_providers"
                );
                connections.push(Connection {
                    supplier: supplier_port.clone(),
                    consumer: consumer_port,
                });
            }
        }
    }
    let mut overridden = BTreeSet::new();
    for change in &source.overrides {
        let parts = change.path.split('/').collect::<Vec<_>>();
        ensure!(
            parts.len() == 6
                && parts[0].is_empty()
                && parts[1] == "instances"
                && parts[3] == "exchanges"
                && parts[5] == "amount"
                && safe_id(parts[2])
                && safe_id(parts[4])
                && change.value.is_finite(),
            "study_unsupported_override"
        );
        ensure!(
            overridden.insert(change.path.clone()),
            "study_duplicate_override"
        );
        let instance = instances
            .iter_mut()
            .find(|i| i.id == parts[2])
            .context("study_override_instance")?;
        ensure!(
            parts[4] != instance.reference_exchange_id,
            "study_reference_override_unsupported"
        );
        instance
            .exchanges
            .iter_mut()
            .find(|e| e.id == parts[4])
            .context("study_override_exchange")?
            .amount = if change.value == 0.0 {
            0.0
        } else {
            change.value
        };
    }
    instances.sort_by(|a, b| a.id.cmp(&b.id));
    connections.sort_by_key(|e| {
        (
            e.supplier.instance_id.clone(),
            e.supplier.exchange_id.clone(),
            e.consumer.instance_id.clone(),
            e.consumer.exchange_id.clone(),
        )
    });
    identities.sort_by_key(|i| {
        (
            i.kind.clone(),
            i.identity.id.clone(),
            i.identity.version.clone(),
        )
    });
    let mut cutoffs = source.cutoffs.clone();
    cutoffs.sort_by_key(|c| (c.port.instance_id.clone(), c.port.exchange_id.clone()));
    let mut seen = BTreeSet::new();
    for c in &cutoffs {
        ensure!(
            !c.reason.trim().is_empty()
                && c.reason.len() <= 4096
                && seen.insert((c.port.instance_id.clone(), c.port.exchange_id.clone())),
            "study_invalid_cutoff"
        );
        let instance = instances
            .iter()
            .find(|i| i.id == c.port.instance_id)
            .context("study_cutoff_instance")?;
        ensure!(
            instance
                .exchanges
                .iter()
                .any(|e| e.id == c.port.exchange_id && e.kind != "Elementary flow"),
            "study_cutoff_exchange"
        );
    }
    let mut findings = source.quality_findings.clone();
    findings.sort();
    findings.dedup();
    Ok((
        NormalizedModel {
            schema_version: "lca.study-normalized-model.v1".into(),
            origin: source.origin.clone(),
            reference,
            instances,
            connections,
            cutoffs,
            overrides: {
                let mut values = source.overrides.clone();
                values.sort_by(|a, b| a.path.cmp(&b.path));
                values
            },
            source_documents: identities,
            quality_findings: findings,
        },
        expanded_bytes.saturating_mul(32),
    ))
}
fn id_for(instance: &Instance) -> String {
    instance.id.clone()
}

fn encoded_size(value: &Value) -> anyhow::Result<usize> {
    struct Counter(usize);
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self
                .0
                .checked_add(bytes.len())
                .ok_or_else(|| std::io::Error::other("study_source_expansion_overflow"))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter(0);
    serde_json::to_writer(&mut counter, value)?;
    Ok(counter.0)
}
