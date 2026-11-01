use super::*;
use std::{fs, path::PathBuf};
fn fixture(layer: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/final-result-v2")
        .join(layer)
}
fn reference(root: &Path) -> QueryArtifactRef {
    serde_json::from_slice(&fs::read(root.join("artifact-ref.json")).unwrap()).unwrap()
}
fn id(n: u64) -> Uuid {
    Uuid::parse_str(&format!("00000000-0000-4000-8000-{n:012}")).unwrap()
}
fn copied() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for entry in fs::read_dir(fixture("lci-lcia")).unwrap() {
        let entry = entry.unwrap();
        fs::copy(entry.path(), dir.path().join(entry.file_name())).unwrap();
    }
    dir
}
fn rebind(root: &Path, name: &str, plain: Option<u64>) -> QueryArtifactRef {
    let mut manifest: FinalManifest =
        serde_json::from_slice(&fs::read(root.join(MANIFEST_NAME)).unwrap()).unwrap();
    let file = manifest
        .files
        .iter_mut()
        .find(|file| file.name == name)
        .unwrap();
    file.byte_size = fs::metadata(root.join(name)).unwrap().len();
    file.sha256 = super::super::super::sha256_file(&root.join(name)).unwrap();
    if let Some(size) = plain {
        file.plain_byte_size = Some(size);
    }
    let bytes = serde_json::to_vec(&manifest).unwrap();
    fs::write(root.join(MANIFEST_NAME), &bytes).unwrap();
    let mut receipt = reference(root);
    receipt.manifest_byte_size = bytes.len() as u64;
    receipt.manifest_sha256 = super::super::super::sha256_file(&root.join(MANIFEST_NAME)).unwrap();
    receipt
}
#[test]
fn release_native_final_result_fixture_preserves_values_versions_and_lineage() {
    let root = fixture("lci-lcia");
    let reader = FinalQueryReader::open(&root, &reference(&root)).unwrap();
    reader.validate_values().unwrap();
    let result = reader.result(id(1), "01.00.000").unwrap();
    assert_eq!(result.relation_kind, "derived");
    assert_eq!(result.sources.len(), 2);
    assert_eq!(
        result.quantitative_reference.value.to_bits(),
        (-2.0_f64).to_bits()
    );
    assert_eq!(
        result.quantitative_reference.direction,
        CompiledExchangeDirection::Input
    );
    assert!(reader.result(id(80), "01.00.000").is_err());
    assert!(reader.result(id(1), "02.00.000").is_err());
    let page = reader.lci(id(1), "01.00.000", 0, 1).unwrap();
    assert_eq!(page.items[0].amount.to_bits(), 2.7_f64.to_bits());
    assert_eq!(page.items[0].item.location.as_deref(), Some("CN"));
    assert_eq!(page.nonzero_count, 3);
    assert_eq!(page.next_offset, Some(1));
    let page = reader.lci(id(1), "01.00.000", 1, 2).unwrap();
    assert_eq!(
        page.items
            .iter()
            .map(|v| v.amount.to_bits())
            .collect::<Vec<_>>(),
        vec![(-3.0_f64).to_bits(), 1e-30_f64.to_bits()]
    );
    assert_eq!(page.next_offset, None);
    assert_eq!(
        reader.lcia(id(1), "01.00.000").unwrap()[0].1.to_bits(),
        42.0_f64.to_bits()
    );
    assert_eq!(
        reader.lcia(id(1), "01.00.001").unwrap()[0].1.to_bits(),
        0.0_f64.to_bits()
    );
    let empty = reader.lci(id(1), "01.00.001", 0, 10).unwrap();
    assert!(empty.items.is_empty());
    assert_eq!(empty.nonzero_count, 0);
    assert!(reader.lci(id(1), "01.00.000", 4, 1).is_err());
}
#[test]
fn release_native_lci_only_is_absent_lcia_not_zero() {
    let root = fixture("lci");
    let reader = FinalQueryReader::open(&root, &reference(&root)).unwrap();
    reader.validate_values().unwrap();
    assert_eq!(reader.manifest.impact_count, 0);
    assert!(reader.lcia(id(10), "01.00.000").is_err());
    assert_eq!(
        reader
            .lci(id(10), "01.00.000", 0, 10)
            .unwrap()
            .nonzero_count,
        3
    );
}
#[test]
fn final_reader_rejects_corrupt_file_and_wrong_format_reference() {
    let dir = copied();
    let root = dir.path();
    let receipt = reference(root);
    let mut wrong = receipt.clone();
    wrong.schema_version = super::super::FORMAT.to_owned();
    assert!(FinalQueryReader::open(root, &wrong).is_err());
    assert!(super::super::QueryReader::open(root, &receipt).is_err());
    let path = root.join("lci.h5");
    let mut bytes = fs::read(&path).unwrap();
    bytes[0] ^= 1;
    fs::write(path, bytes).unwrap();
    assert!(FinalQueryReader::open(root, &receipt).is_err());
}
#[test]
fn final_reader_rejects_duplicate_result_identity_and_incomplete_index() {
    let dir = copied();
    let root = dir.path();
    let manifest: FinalManifest =
        serde_json::from_slice(&fs::read(root.join(MANIFEST_NAME)).unwrap()).unwrap();
    let mut rows: Vec<FinalResultEntry> = read_index(root, &manifest, "results.json.gz").unwrap();
    rows[1].version = rows[0].version.clone();
    let bytes = serde_json::to_vec(&rows).unwrap();
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    std::io::Write::write_all(&mut encoder, &bytes).unwrap();
    fs::write(root.join("results.json.gz"), encoder.finish().unwrap()).unwrap();
    let receipt = rebind(root, "results.json.gz", Some(bytes.len() as u64));
    assert!(FinalQueryReader::open(root, &receipt).is_err());
    let receipt = rebind(root, "results.json.gz", Some(bytes.len() as u64 + 1));
    assert!(FinalQueryReader::open(root, &receipt).is_err());
}
#[test]
fn final_csr_validation_catches_duplicate_across_page_boundary() {
    let dir = copied();
    let root = dir.path();
    {
        let file = hdf5::File::open_rw(root.join("lci.h5")).unwrap();
        file.dataset("indices")
            .unwrap()
            .write_raw(&[0_u64, 0, 2])
            .unwrap();
    }
    let receipt = rebind(root, "lci.h5", None);
    let reader = FinalQueryReader::open(root, &receipt).unwrap();
    assert!(reader.lci(id(1), "01.00.000", 0, 1).is_ok());
    assert!(reader.lci(id(1), "01.00.000", 1, 1).is_err());
    assert!(reader.validate_values().is_err());
}
