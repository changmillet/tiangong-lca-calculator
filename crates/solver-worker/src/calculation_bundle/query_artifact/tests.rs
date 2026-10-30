use super::*;
use crate::calculation_bundle::{BuiltCalculationBundle, tests::fixture_writer};
use solver_core::{FactorizationState, SolveResult};

pub(crate) fn fixture(scale: f64) -> BuiltCalculationBundle {
    let mut writer = fixture_writer();
    add_frozen_flow(&mut writer);
    writer
        .write_result_chunk(
            0,
            &[SolveResult {
                x: Some(vec![scale]),
                g: None,
                h: Some((0..25).map(|index| f64::from(index) * scale).collect()),
                factorization_state: FactorizationState::Ready,
            }],
        )
        .unwrap();
    writer.finish().unwrap()
}
pub(crate) fn receipt(bundle: &BuiltCalculationBundle) -> CalculationBundleArtifactRef {
    CalculationBundleArtifactRef {
        schema_version: super::super::CALCULATION_BUNDLE_FORMAT.to_owned(),
        calculation_id: bundle.calculation_id,
        bundle_content_hash: bundle.bundle_content_hash.clone(),
        manifest_url: "https://example.invalid/calculation-bundle.json".to_owned(),
        manifest_sha256: bundle.manifest_sha256.clone(),
        manifest_byte_size: bundle.manifest_byte_size,
        artifact_count: bundle.artifacts.len(),
        downloads: vec![],
    }
}

#[test]
fn canonical_numeric_round_trip_keeps_sign_zero_and_small_values() {
    for scale in [0.0, -2.0, 1e-20] {
        let source = fixture(scale);
        let mut expected_lcia = Vec::new();
        for file in source
            .artifacts
            .iter()
            .filter(|file| file.metadata.kind == "lcia")
        {
            super::super::visit_gzip_ndjson::<super::super::LciaRecord, _>(
                &file.local_path,
                |record| {
                    expected_lcia.push(record.mean_amount);
                    Ok(())
                },
            )
            .unwrap();
        }
        let built = build(
            source.manifest_path.parent().unwrap(),
            &receipt(&source),
            Uuid::new_v4(),
            &BuildLimits::default(),
        )
        .unwrap();
        let reader = QueryReader::open(built.path(), &built.reference).unwrap();
        let id = Uuid::parse_str("11111111-1111-4111-8111-111111111111").unwrap();
        let impacts = reader.lcia(id, "01.00.000").unwrap();
        assert_eq!(impacts.len(), 25);
        for (index, (_, value)) in impacts.iter().enumerate() {
            assert_eq!(value.to_bits(), expected_lcia[index].to_bits());
        }
        let lci = reader.lci(id, "01.00.000", 0, 100).unwrap();
        if scale == 0.0 {
            assert!(lci.items.is_empty());
        } else {
            assert!(!lci.items.is_empty());
            assert!(
                lci.items
                    .iter()
                    .all(|item| item.amount.signum() == scale.signum())
            );
        }
        assert!(reader.process(id, "02.00.000").is_err());
        assert!(reader.lci(id, "01.00.000", 0, MAX_PAGE_SIZE + 1).is_err());
    }
}

#[test]
fn rejects_corrupt_source_and_query_file_and_preserves_existing_output() {
    let source = fixture(1.0);
    let expected = receipt(&source);
    let built = build(
        source.manifest_path.parent().unwrap(),
        &expected,
        Uuid::new_v4(),
        &BuildLimits::default(),
    )
    .unwrap();
    let output = tempfile::tempdir().unwrap();
    assert!(built.persist(output.path()).is_err());
    let file = built.path().join("lci.h5");
    std::fs::write(&file, b"corrupt").unwrap();
    assert!(QueryReader::open(built.path(), &built.reference).is_err());
    let mut wrong = expected;
    wrong.manifest_sha256 = "0".repeat(64);
    assert!(
        build(
            source.manifest_path.parent().unwrap(),
            &wrong,
            Uuid::new_v4(),
            &BuildLimits::default()
        )
        .is_err()
    );
}

fn add_frozen_flow(writer: &mut super::super::CalculationBundleWriter) {
    use super::super::{DeterministicGzipNdjsonWriter, canonical_sha256, visit_gzip_ndjson};
    let source = writer
        .artifacts
        .iter_mut()
        .find(|file| file.metadata.kind == "source_closure")
        .unwrap();
    let mut records = Vec::<serde_json::Value>::new();
    visit_gzip_ndjson(&source.local_path, |value| {
        records.push(value);
        Ok(())
    })
    .unwrap();
    let flow = serde_json::json!({"flowDataSet":{"flowInformation":{"dataSetInformation":{"common:UUID":"33333333-3333-4333-8333-333333333333", "name":{"baseName":{"@xml:lang":"en","#text":"Synthetic elementary flow"}}}}}});
    records.push(serde_json::json!({"schemaVersion":"tiangong.source-closure.dataset.v1","datasetType":"flow","role":"support","uuid":"33333333-3333-4333-8333-333333333333","version":"01.00.000","path":"flows/synthetic.json","sha256":canonical_sha256(&flow).unwrap(),"document":flow}));
    let mut output = DeterministicGzipNdjsonWriter::create(&source.local_path).unwrap();
    for record in records {
        output.write(&record).unwrap();
    }
    let result = output.finish().unwrap();
    source.metadata.sha256 = result.sha256;
    source.metadata.uncompressed_sha256 = Some(result.uncompressed_sha256);
    source.metadata.byte_size = result.byte_size;
    source.metadata.uncompressed_byte_size = Some(result.uncompressed_byte_size);
    source.metadata.record_count = result.record_count;
}

fn refresh_manifest(bundle: &mut BuiltCalculationBundle) {
    bundle.manifest.artifacts = bundle
        .artifacts
        .iter()
        .map(|file| file.metadata.clone())
        .collect();
    bundle
        .manifest
        .artifacts
        .sort_unstable_by(|a, b| a.path.cmp(&b.path));
    bundle.bundle_content_hash = super::super::bundle_content_hash(&bundle.manifest).unwrap();
    bundle
        .manifest
        .bundle_content_hash
        .clone_from(&bundle.bundle_content_hash);
    std::fs::write(
        &bundle.manifest_path,
        super::super::canonical_json_bytes(&bundle.manifest).unwrap(),
    )
    .unwrap();
    bundle.manifest_sha256 = super::super::sha256_file(&bundle.manifest_path).unwrap();
    bundle.manifest_byte_size = std::fs::metadata(&bundle.manifest_path).unwrap().len();
}

fn records(file: &super::super::LocalCalculationBundleArtifact) -> Vec<serde_json::Value> {
    let mut output = Vec::new();
    super::super::visit_gzip_ndjson(&file.local_path, |record| {
        output.push(record);
        Ok(())
    })
    .unwrap();
    output
}

fn write_records(
    file: &mut super::super::LocalCalculationBundleArtifact,
    rows: &[serde_json::Value],
) {
    let mut writer = super::super::DeterministicGzipNdjsonWriter::create(&file.local_path).unwrap();
    for row in rows {
        writer.write(row).unwrap();
    }
    let finished = writer.finish().unwrap();
    file.metadata.sha256 = finished.sha256;
    file.metadata.byte_size = finished.byte_size;
    file.metadata.uncompressed_sha256 = Some(finished.uncompressed_sha256);
    file.metadata.uncompressed_byte_size = Some(finished.uncompressed_byte_size);
    file.metadata.record_count = finished.record_count;
}

#[test]
fn rejects_missing_duplicate_and_out_of_order_records_even_when_hashes_match() {
    for case in [
        "missing_lcia",
        "duplicate_lcia",
        "duplicate_lci",
        "wrong_process",
        "explicit_zero",
    ] {
        let mut bundle = fixture(1.0);
        let kind = if case.ends_with("lcia") {
            "lcia"
        } else {
            "lci"
        };
        let file = bundle
            .artifacts
            .iter_mut()
            .find(|file| file.metadata.kind == kind)
            .unwrap();
        let mut rows = records(file);
        match case {
            "missing_lcia" => {
                rows.pop();
            }
            "duplicate_lcia" => {
                rows[1] = rows[0].clone();
            }
            "duplicate_lci" => {
                rows.push(rows[0].clone());
            }
            "wrong_process" => {
                rows[0]["processIndex"] = serde_json::json!(1);
            }
            "explicit_zero" => {
                rows[0]["meanAmount"] = serde_json::json!(0);
            }
            _ => unreachable!(),
        }
        write_records(file, &rows);
        refresh_manifest(&mut bundle);
        assert!(
            build(
                bundle.manifest_path.parent().unwrap(),
                &receipt(&bundle),
                Uuid::new_v4(),
                &BuildLimits::default()
            )
            .is_err(),
            "{case}"
        );
    }
    let mut bundle = fixture(0.0);
    bundle.artifacts.retain(|file| file.metadata.kind != "lci");
    refresh_manifest(&mut bundle);
    assert!(
        build(
            bundle.manifest_path.parent().unwrap(),
            &receipt(&bundle),
            Uuid::new_v4(),
            &BuildLimits::default()
        )
        .is_err()
    );
}

fn many_processes(count: usize) -> BuiltCalculationBundle {
    use super::super::LocalCalculationBundleArtifact;
    let mut bundle = fixture(1.0);
    for kind in ["process_axis", "biosphere_edges", "lci", "lcia"] {
        let original = bundle
            .artifacts
            .iter()
            .find(|file| file.metadata.kind == kind)
            .unwrap();
        let templates = records(original);
        let metadata = original.metadata.clone();
        bundle.artifacts.retain(|file| file.metadata.kind != kind);
        for first in (0..count).step_by(256) {
            let last = (first + 255).min(count - 1);
            let mut rows = Vec::new();
            for index in first..=last {
                for template in &templates {
                    if kind == "lci" && index % 7 == 0 {
                        continue;
                    }
                    let mut row = template.clone();
                    row["processIndex"] = serde_json::json!(index);
                    if kind == "process_axis" {
                        row["rootProcess"]["id"] =
                            serde_json::json!(Uuid::from_u128((index.max(1)) as u128).to_string());
                        if index == 1 {
                            row["rootProcess"]["version"] = serde_json::json!("02.00.000");
                        }
                    }
                    if matches!(kind, "lci" | "lcia") {
                        row["meanAmount"] = serde_json::json!(
                            f64::from(u32::try_from(index).unwrap())
                                + template["meanAmount"].as_f64().unwrap()
                        );
                    }
                    rows.push(row);
                }
            }
            let name = format!("query-test-{kind}-{first}.ndjson.gz");
            let mut file = LocalCalculationBundleArtifact {
                metadata: metadata.clone(),
                local_path: bundle.manifest_path.parent().unwrap().join(&name),
            };
            file.metadata.path = name;
            file.metadata.first_process_index = Some(first);
            file.metadata.last_process_index = Some(last);
            write_records(&mut file, &rows);
            bundle.artifacts.push(file);
        }
    }
    bundle.manifest.snapshot.process_count = count;
    bundle.manifest.scope.process_count = count;
    refresh_manifest(&mut bundle);
    bundle
}

#[test]
fn crosses_chunk_boundaries_and_keeps_exact_versions_and_zero_rows() {
    let bundle = many_processes(258);
    let built = build(
        bundle.manifest_path.parent().unwrap(),
        &receipt(&bundle),
        Uuid::new_v4(),
        &BuildLimits::default(),
    )
    .unwrap();
    let reader = QueryReader::open(built.path(), &built.reference).unwrap();
    assert_eq!(
        reader
            .process(Uuid::from_u128(1), "01.00.000")
            .unwrap()
            .ordinal,
        0
    );
    assert_eq!(
        reader
            .process(Uuid::from_u128(1), "02.00.000")
            .unwrap()
            .ordinal,
        1
    );
    for index in [0, 1, 7, 255, 256, 257] {
        let version = if index == 1 { "02.00.000" } else { "01.00.000" };
        let id = Uuid::from_u128(index.max(1));
        let impacts = reader.lcia(id, version).unwrap();
        assert_eq!(
            impacts[0].1.to_bits(),
            f64::from(u32::try_from(index).unwrap()).to_bits()
        );
        let page = reader.lci(id, version, 0, 1).unwrap();
        assert_eq!(page.items.is_empty(), index % 7 == 0);
    }
}

#[test]
fn refuses_decompression_and_dense_output_over_budget() {
    let bundle = fixture(1.0);
    for limits in [
        BuildLimits {
            max_plain_file_bytes: 1,
            ..BuildLimits::default()
        },
        BuildLimits {
            max_output_bytes: 1,
            ..BuildLimits::default()
        },
    ] {
        assert!(
            build(
                bundle.manifest_path.parent().unwrap(),
                &receipt(&bundle),
                Uuid::new_v4(),
                &limits
            )
            .is_err()
        );
    }
}

#[test]
fn lci_pagination_retains_direction_and_location_identity() {
    let mut bundle = fixture(1.0);
    for kind in ["biosphere_edges", "lci"] {
        let file = bundle
            .artifacts
            .iter_mut()
            .find(|file| file.metadata.kind == kind)
            .unwrap();
        let original = records(file)[0].clone();
        let mut input = original.clone();
        input["direction"] = serde_json::json!("Input");
        input["meanAmount"] = serde_json::json!(-1e-20);
        let mut other_location = original.clone();
        other_location["location"] = serde_json::json!("CN");
        other_location["meanAmount"] = serde_json::json!(2.0);
        write_records(file, &[input, other_location, original]);
    }
    refresh_manifest(&mut bundle);
    let built = build(
        bundle.manifest_path.parent().unwrap(),
        &receipt(&bundle),
        Uuid::new_v4(),
        &BuildLimits::default(),
    )
    .unwrap();
    let reader = QueryReader::open(built.path(), &built.reference).unwrap();
    let id = Uuid::parse_str("11111111-1111-4111-8111-111111111111").unwrap();
    let mut pages = Vec::new();
    for offset in 0..3 {
        let page = reader.lci(id, "01.00.000", offset, 1).unwrap();
        assert_eq!(page.nonzero_count, 3);
        assert_eq!(page.next_offset, (offset < 2).then_some(offset + 1));
        pages.push(page.items.into_iter().next().unwrap());
    }
    assert_eq!(
        pages[0].item.key.direction,
        crate::compiled_graph::CompiledExchangeDirection::Input
    );
    assert!(pages[0].amount < 0.0);
    assert_eq!(pages[1].item.key.location.as_deref(), Some("CN"));
    assert_eq!(pages[2].item.key.location.as_deref(), Some("GLO"));
    assert!(reader.lci(id, "01.00.000", 4, 1).is_err());
}

/// Explicit local qualification only: this small sparse fixture is not production data.
#[test]
#[ignore = "local synthetic timing; run explicitly with --ignored --nocapture"]
fn synthetic_query_layout_timing() {
    use std::time::Instant;
    let bundle = many_processes(4096);
    let started = Instant::now();
    let built = build(
        bundle.manifest_path.parent().unwrap(),
        &receipt(&bundle),
        Uuid::new_v4(),
        &BuildLimits::default(),
    )
    .unwrap();
    let build_ms = started.elapsed().as_millis();
    let started = Instant::now();
    let reader = QueryReader::open(built.path(), &built.reference).unwrap();
    let open_ms = started.elapsed().as_millis();
    let mut durations = Vec::new();
    for index in 2..1002 {
        let started = Instant::now();
        let id = Uuid::from_u128(index);
        assert_eq!(reader.lcia(id, "01.00.000").unwrap().len(), 25);
        reader.lci(id, "01.00.000", 0, 100).unwrap();
        durations.push(started.elapsed().as_micros());
    }
    durations.sort_unstable();
    eprintln!(
        "synthetic HDF5: processes={}, impacts={}, inventory={}, nonzeros={}, bytes={}, build_ms={}, verified_local_open_ms={}, paired_hot_read_us_p50={}, p95={}; excludes S3, HTTP and production-scale LCI density",
        reader.manifest.process_count,
        reader.manifest.impact_count,
        reader.manifest.inventory_count,
        reader.manifest.lci_nonzero_count,
        reader
            .manifest
            .files
            .iter()
            .map(|file| file.byte_size)
            .sum::<u64>(),
        build_ms,
        open_ms,
        durations[499],
        durations[949]
    );
}
