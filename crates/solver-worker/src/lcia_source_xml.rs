//! Bounded, local ILCD XML inspection. The projection is evidence; the JSON adapter is not.
//!
//! Canonicalization uses `canonical_json_sha256`: object keys are sorted and every array
//! remains ordered. The document `content` retains declarations, processing instructions,
//! comments and whitespace. Elements retain lexical `QName`, expanded namespace/local name,
//! ordered attributes (raw lexical and XML 1.0 normalized decoded values), mixed content
//! and empty-tag spelling. Attribute normalization converts literal tab/CR/LF to spaces
//! (CRLF to one space) and expands predefined/numeric entities; numeric character
//! references retain the referenced character. `raw_value` preserves the original UTF-8
//! attribute content, including entity spellings and literal whitespace, without quotes.
//! Text and CDATA retain their exact UTF-8 contents, including line endings. Entity-reference
//! events retain both spelling and decoded value. Quotes/spacing inside markup are represented
//! semantically rather than byte-for-byte; `raw_sha256` separately binds all original bytes.
//! XML 1.0 UTF-8 only; DTDs, declared entities and unresolved references are rejected. No
//! filesystem, URI resolution, network access, source rewriting or scientific inference occurs.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, ensure};
use quick_xml::{
    XmlVersion,
    events::{BytesStart, Event},
    name::ResolveResult,
    reader::NsReader,
};
use serde::Serialize;
use serde_json::{Map, Value, json};
use uuid::Uuid;

use crate::calculation_evidence::{canonical_json_sha256, sha256_bytes};

pub const XML_PROJECTION_SCHEMA_VERSION: &str = "lcia.ilcd-xml-projection.v1";
pub const XML_CANONICALIZATION: &str = "ilcd-xml-ordered-content-qnames-attributes-utf8.v1";
const ILCD_NS: &str = "http://lca.jrc.it/ILCD/LCIAMethod";
const COMMON_NS: &str = "http://lca.jrc.it/ILCD/Common";
const XMLNS_NS: &str = "http://www.w3.org/2000/xmlns/";
pub const MAX_XML_BYTES: usize = 64 * 1024 * 1024;
const MAX_DEPTH: usize = 64;
const MAX_NODES: usize = 2_000_000;
const MAX_ATTRIBUTES: usize = 256;
const MAX_CONTENT_BYTES: usize = 64 * 1024 * 1024;
const MAX_PROJECTION_BYTES: usize = 512 * 1024 * 1024;
const MAX_NAME_BYTES: usize = 1_024;
const MAX_NAMESPACE_BYTES: usize = 8_192;

#[derive(Debug, Clone, Serialize)]
pub struct MetadataIndexEntry {
    /// Expanded element-name path; repeated siblings are not disambiguated here.
    pub xml_path: String,
    /// Exact RFC 6901 pointer into `projection`, including ordered content indices.
    pub pointer: String,
    pub sha256: String,
    /// Direct character content only; `None` when element children are present.
    pub text: Option<String>,
}

#[derive(Debug, Clone)]
pub struct InspectedIlcdMethod {
    pub method_id: Uuid,
    pub method_version: String,
    pub raw_sha256: String,
    pub projection: Value,
    pub canonical_body_sha256: String,
    pub ordered_factors: Vec<Value>,
    pub ordered_factors_sha256: String,
    /// In-memory compatibility adapter. Full projection/subtree hashes remain authoritative.
    pub method_document: Value,
    pub metadata_index: Vec<MetadataIndexEntry>,
}

fn namespace(result: ResolveResult<'_>) -> anyhow::Result<Option<String>> {
    match result {
        ResolveResult::Unbound => Ok(None),
        ResolveResult::Bound(value) => {
            ensure!(
                value.as_ref().len() <= MAX_NAMESPACE_BYTES,
                "lcia_xml_namespace_size_limit"
            );
            Ok(Some(value.as_ref().to_owned()))
        }
        ResolveResult::Unknown(prefix) => anyhow::bail!("lcia_xml_unknown_namespace: {prefix}"),
    }
}

fn valid_xml_chars(value: &str) -> bool {
    value.chars().all(|c| {
        matches!(c, '\t' | '\n' | '\r') || c >= '\u{20}' && c != '\u{fffe}' && c != '\u{ffff}'
    })
}

fn element(reader: &NsReader<&[u8]>, start: &BytesStart<'_>, empty: bool) -> anyhow::Result<Value> {
    ensure!(
        start.name().as_ref().len() <= MAX_NAME_BYTES,
        "lcia_xml_name_size_limit"
    );
    let (resolved, local) = reader.resolver().resolve_element(start.name());
    let namespace_uri = namespace(resolved)?;
    let mut attributes = Vec::new();
    let mut unique = BTreeSet::new();
    for attribute in start.attributes() {
        let attribute = attribute?;
        ensure!(
            attributes.len() < MAX_ATTRIBUTES,
            "lcia_xml_attribute_limit"
        );
        let name = attribute.key.as_ref();
        ensure!(
            name.len() <= MAX_NAME_BYTES,
            "lcia_xml_attribute_name_size_limit"
        );
        if name == "xmlns" || name.starts_with("xmlns:") {
            ensure!(
                attribute.value.len() <= MAX_NAMESPACE_BYTES,
                "lcia_xml_namespace_size_limit"
            );
        }
        let (namespace_uri, local_name) = if name == "xmlns" {
            (Some(XMLNS_NS.to_owned()), "xmlns".to_owned())
        } else if let Some(prefix) = name.strip_prefix("xmlns:") {
            (Some(XMLNS_NS.to_owned()), prefix.to_owned())
        } else {
            let (resolved, local) = reader.resolver().resolve_attribute(attribute.key);
            (namespace(resolved)?, local.as_ref().to_owned())
        };
        ensure!(
            unique.insert((namespace_uri.clone(), local_name.clone())),
            "lcia_xml_duplicate_expanded_attribute"
        );
        let value = attribute
            .normalized_value(XmlVersion::Implicit1_0)?
            .into_owned();
        ensure!(
            valid_xml_chars(&value),
            "lcia_xml_invalid_attribute_character"
        );
        attributes.push(json!({"name":name,"namespace_uri":namespace_uri,
            "local_name":local_name,"raw_value":attribute.value,"value":value}));
    }
    Ok(json!({"kind":"element","name":start.name().as_ref(),
        "namespace_uri":namespace_uri,"local_name":local.as_ref(),
        "attributes":attributes,"content":[],"empty":empty}))
}

fn append(stack: &mut [Value], content: &mut Vec<Value>, node: Value) {
    if let Some(parent) = stack.last_mut() {
        parent["content"]
            .as_array_mut()
            .expect("element content")
            .push(node);
    } else {
        content.push(node);
    }
}

// Keep the complete event/security dispatch together so retained and rejected events are auditable.
#[allow(clippy::too_many_lines)]
fn parse(bytes: &[u8]) -> anyhow::Result<Value> {
    ensure!(bytes.len() <= MAX_XML_BYTES, "lcia_xml_input_size_limit");
    let input = std::str::from_utf8(bytes).context("lcia_xml_requires_utf8")?;
    let input = input.strip_prefix('\u{feff}').unwrap_or(input);
    ensure!(valid_xml_chars(input), "lcia_xml_invalid_character");
    let mut reader = NsReader::from_str(input);
    reader.config_mut().check_comments = true;
    reader.config_mut().check_end_names = true;
    let mut stack = Vec::new();
    let mut content = Vec::new();
    let mut nodes = 0_usize;
    let mut content_bytes = 0_usize;
    let mut expanded_bytes = 0_usize;
    let mut declaration_seen = false;
    loop {
        let event = reader.read_event().context("lcia_xml_malformed")?;
        nodes += 1;
        ensure!(nodes <= MAX_NODES, "lcia_xml_node_limit");
        match event {
            Event::Start(start) => {
                ensure!(stack.len() < MAX_DEPTH, "lcia_xml_depth_limit");
                let node = element(&reader, &start, false)?;
                expanded_bytes += serde_json::to_vec(&node)?.len();
                ensure!(
                    expanded_bytes <= MAX_PROJECTION_BYTES,
                    "lcia_xml_projection_size_limit"
                );
                stack.push(node);
            }
            Event::Empty(start) => {
                ensure!(stack.len() < MAX_DEPTH, "lcia_xml_depth_limit");
                let node = element(&reader, &start, true)?;
                expanded_bytes += serde_json::to_vec(&node)?.len();
                ensure!(
                    expanded_bytes <= MAX_PROJECTION_BYTES,
                    "lcia_xml_projection_size_limit"
                );
                append(&mut stack, &mut content, node);
            }
            Event::End(_) => {
                let node = stack.pop().context("lcia_xml_unmatched_end")?;
                append(&mut stack, &mut content, node);
            }
            Event::DocType(_) => anyhow::bail!("lcia_xml_dtd_entities_forbidden"),
            Event::Decl(declaration) => {
                ensure!(
                    stack.is_empty() && content.is_empty() && !declaration_seen,
                    "lcia_xml_misplaced_declaration"
                );
                declaration_seen = true;
                ensure!(
                    declaration.version()?.as_ref() == "1.0",
                    "lcia_xml_requires_version_1_0"
                );
                if let Some(encoding) = declaration.encoding() {
                    ensure!(
                        encoding?.eq_ignore_ascii_case("utf-8"),
                        "lcia_xml_requires_utf8"
                    );
                }
                content.push(json!({"kind":"declaration","value":String::from(&*declaration)}));
            }
            Event::Text(text) => {
                let value = text.into_inner().into_owned();
                if stack.is_empty() {
                    ensure!(value.trim().is_empty(), "lcia_xml_text_outside_root");
                }
                content_bytes += value.len();
                append(
                    &mut stack,
                    &mut content,
                    json!({"kind":"text","value":value}),
                );
            }
            Event::CData(text) => {
                ensure!(!stack.is_empty(), "lcia_xml_cdata_outside_root");
                let value = text.into_inner().into_owned();
                content_bytes += value.len();
                append(
                    &mut stack,
                    &mut content,
                    json!({"kind":"cdata","value":value}),
                );
            }
            Event::GeneralRef(reference) => {
                ensure!(!stack.is_empty(), "lcia_xml_entity_outside_root");
                let name = reference.into_inner().into_owned();
                let value = quick_xml::escape::unescape(&format!("&{name};"))?.into_owned();
                ensure!(valid_xml_chars(&value), "lcia_xml_invalid_entity_character");
                content_bytes += value.len();
                append(
                    &mut stack,
                    &mut content,
                    json!({"kind":"entity_reference","name":name,"value":value}),
                );
            }
            Event::Comment(comment) => {
                let value = comment.into_inner().into_owned();
                content_bytes += value.len();
                append(
                    &mut stack,
                    &mut content,
                    json!({"kind":"comment","value":value}),
                );
            }
            Event::PI(instruction) => {
                let value = instruction.into_inner().into_owned();
                content_bytes += value.len();
                append(
                    &mut stack,
                    &mut content,
                    json!({"kind":"processing_instruction","value":value}),
                );
            }
            Event::Eof => break,
        }
        ensure!(content_bytes <= MAX_CONTENT_BYTES, "lcia_xml_text_limit");
        ensure!(
            expanded_bytes + content_bytes <= MAX_PROJECTION_BYTES,
            "lcia_xml_projection_size_limit"
        );
    }
    ensure!(stack.is_empty(), "lcia_xml_unclosed_element");
    ensure!(
        content
            .iter()
            .filter(|node| node["kind"] == "element")
            .count()
            == 1,
        "lcia_xml_requires_one_root"
    );
    Ok(
        json!({"schema_version":XML_PROJECTION_SCHEMA_VERSION,"canonicalization":XML_CANONICALIZATION,"content":content}),
    )
}

fn children(node: &Value) -> impl Iterator<Item = &Value> {
    node["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|child| child["kind"] == "element")
}

fn child<'a>(node: &'a Value, namespace_uri: &str, local_name: &str) -> anyhow::Result<&'a Value> {
    let mut matches = children(node).filter(|child| {
        child["namespace_uri"] == namespace_uri && child["local_name"] == local_name
    });
    let value = matches
        .next()
        .with_context(|| format!("lcia_xml_required_element_missing: {local_name}"))?;
    ensure!(
        matches.next().is_none(),
        "lcia_xml_ambiguous_element: {local_name}"
    );
    Ok(value)
}

fn scalar(node: &Value) -> anyhow::Result<String> {
    ensure!(
        children(node).next().is_none(),
        "lcia_xml_scalar_has_element_children"
    );
    Ok(node["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|item| {
            matches!(
                item["kind"].as_str(),
                Some("text" | "cdata" | "entity_reference")
            )
        })
        .filter_map(|item| item["value"].as_str())
        .collect())
}

fn adapter_key(node: &Value) -> String {
    let local = node["local_name"].as_str().expect("projected local name");
    match node["namespace_uri"].as_str() {
        Some(ILCD_NS) | None => local.to_owned(),
        Some(COMMON_NS) => format!("common:{local}"),
        Some(uri) => format!("{{{uri}}}{local}"),
    }
}

fn adapter(node: &Value) -> anyhow::Result<Value> {
    let mut fields = Map::new();
    for attribute in node["attributes"].as_array().into_iter().flatten() {
        if attribute["namespace_uri"] == XMLNS_NS {
            continue;
        }
        let key = format!("@{}", attribute["name"].as_str().expect("attribute name"));
        ensure!(
            fields.insert(key, attribute["value"].clone()).is_none(),
            "lcia_xml_adapter_duplicate_attribute"
        );
    }
    let mut grouped = BTreeMap::<String, Vec<Value>>::new();
    for child in children(node) {
        grouped
            .entry(adapter_key(child))
            .or_default()
            .push(adapter(child)?);
    }
    if fields.is_empty() && grouped.is_empty() {
        return Ok(Value::String(scalar(node)?));
    }
    for (key, mut values) in grouped {
        let value = if values.len() == 1 {
            values.pop().expect("one child")
        } else {
            Value::Array(values)
        };
        ensure!(
            fields.insert(key, value).is_none(),
            "lcia_xml_adapter_duplicate_field"
        );
    }
    if children(node).next().is_none() {
        let text = scalar(node)?;
        if !text.is_empty() {
            fields.insert("#text".to_owned(), Value::String(text));
        }
    }
    Ok(Value::Object(fields))
}

fn index(
    node: &Value,
    pointer: &str,
    parent_path: &str,
    entries: &mut Vec<MetadataIndexEntry>,
) -> anyhow::Result<()> {
    if node["kind"] != "element" {
        return Ok(());
    }
    let path = format!(
        "{parent_path}/{{{}}}{}",
        node["namespace_uri"].as_str().unwrap_or(""),
        node["local_name"].as_str().expect("local name")
    );
    entries.push(MetadataIndexEntry {
        xml_path: path.clone(),
        pointer: pointer.to_owned(),
        sha256: canonical_json_sha256(node)?,
        text: if children(node).next().is_none() {
            Some(scalar(node)?)
        } else {
            None
        },
    });
    // Inspection reports expose method metadata, not a second exported real CF dataset.
    // Full factor subtrees remain available only in the caller's in-memory projection.
    if node["namespace_uri"] == ILCD_NS && node["local_name"] == "characterisationFactors" {
        return Ok(());
    }
    for (offset, child) in node["content"].as_array().into_iter().flatten().enumerate() {
        index(
            child,
            &format!("{pointer}/content/{offset}"),
            &path,
            entries,
        )?;
    }
    Ok(())
}

pub fn inspect_ilcd_method(bytes: &[u8]) -> anyhow::Result<InspectedIlcdMethod> {
    let projection = parse(bytes)?;
    let (root_offset, root) = projection["content"]
        .as_array()
        .expect("document content")
        .iter()
        .enumerate()
        .find(|(_, node)| node["kind"] == "element")
        .expect("one root");
    ensure!(
        root["namespace_uri"] == ILCD_NS && root["local_name"] == "LCIAMethodDataSet",
        "lcia_xml_wrong_method_root"
    );
    let information = child(root, ILCD_NS, "LCIAMethodInformation")?;
    let identity = child(information, ILCD_NS, "dataSetInformation")?;
    let method_id = Uuid::parse_str(&scalar(child(identity, COMMON_NS, "UUID")?)?)?;
    let administrative = child(root, ILCD_NS, "administrativeInformation")?;
    let publication = child(administrative, ILCD_NS, "publicationAndOwnership")?;
    let method_version = scalar(child(publication, COMMON_NS, "dataSetVersion")?)?;
    crate::calculation_bundle::validate_version(&method_version, "lcia_xml.method_version")?;
    let factor_container = child(root, ILCD_NS, "characterisationFactors")?;
    let ordered_factors = children(factor_container)
        .filter(|node| node["namespace_uri"] == ILCD_NS && node["local_name"] == "factor")
        .cloned()
        .collect::<Vec<_>>();
    ensure!(
        children(factor_container).count() == ordered_factors.len(),
        "lcia_xml_unknown_factor_container_element"
    );
    // Validate unique, namespace-correct mandatory CF fields before compatibility projection.
    for factor in &ordered_factors {
        for field in ["referenceToFlowDataSet", "exchangeDirection", "meanValue"] {
            child(factor, ILCD_NS, field)?;
        }
        ensure!(
            children(factor)
                .filter(|node| node["namespace_uri"] == ILCD_NS && node["local_name"] == "location")
                .count()
                <= 1,
            "lcia_xml_ambiguous_location"
        );
    }
    let mut method_document = json!({"LCIAMethodDataSet":adapter(root)?});
    // Preserve a stable factor array even when zero/one original factors exist.
    if method_document["LCIAMethodDataSet"]["characterisationFactors"].is_string() {
        method_document["LCIAMethodDataSet"]["characterisationFactors"] = json!({});
    }
    method_document["LCIAMethodDataSet"]["characterisationFactors"]["factor"] = Value::Array(
        ordered_factors
            .iter()
            .map(adapter)
            .collect::<anyhow::Result<Vec<_>>>()?,
    );
    let mut metadata_index = Vec::new();
    index(
        root,
        &format!("/content/{root_offset}"),
        "",
        &mut metadata_index,
    )?;
    Ok(InspectedIlcdMethod {
        method_id,
        method_version,
        raw_sha256: sha256_bytes(bytes),
        canonical_body_sha256: canonical_json_sha256(&projection)?,
        ordered_factors_sha256: canonical_json_sha256(&Value::Array(ordered_factors.clone()))?,
        projection,
        ordered_factors,
        method_document,
        metadata_index,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn method(factors: &str) -> String {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><?source keep?><LCIAMethodDataSet xmlns="{ILCD_NS}" xmlns:c="{COMMON_NS}" xmlns:x="urn:unknown" x:flag="keep"><LCIAMethodInformation><dataSetInformation><c:UUID>00000000-0000-0000-0000-000000000339</c:UUID><c:intendedApplications xml:lang="en">Global &amp; exact</c:intendedApplications><!--retain--></dataSetInformation></LCIAMethodInformation><administrativeInformation><publicationAndOwnership><c:dataSetVersion>01.00.000</c:dataSetVersion></publicationAndOwnership></administrativeInformation><characterisationFactors>{factors}</characterisationFactors></LCIAMethodDataSet>"#
        )
    }

    fn factor(location: &str) -> String {
        format!(
            r#"<factor><referenceToFlowDataSet refObjectId="00000000-0000-0000-0000-000000000340" version="03.00.004" uri="../flows/not-followed.xml"><c:shortDescription xml:lang="en">flow</c:shortDescription></referenceToFlowDataSet>{location}<exchangeDirection>Output</exchangeDirection><meanValue>4.0</meanValue><x:condition state="unknown">preserve</x:condition><generalComment/></factor>"#
        )
    }

    #[test]
    fn retains_full_projection_repeated_factors_and_inspectable_basis() {
        let source = method(&format!("{}{}", factor("<location/>"), factor("")));
        let inspected = inspect_ilcd_method(source.as_bytes()).expect("native source inspection");
        assert_eq!(inspected.method_version, "01.00.000");
        assert_eq!(inspected.ordered_factors.len(), 2);
        assert_eq!(inspected.raw_sha256, sha256_bytes(source.as_bytes()));
        assert_ne!(inspected.ordered_factors[0], inspected.ordered_factors[1]);
        let rows =
            &inspected.method_document["LCIAMethodDataSet"]["characterisationFactors"]["factor"];
        assert_eq!(rows[0]["location"], "");
        assert!(rows[1].get("location").is_none());
        assert_eq!(
            rows[0]["referenceToFlowDataSet"]["@uri"],
            "../flows/not-followed.xml"
        );
        assert_eq!(rows[0]["{urn:unknown}condition"]["@state"], "unknown");
        let basis = inspected
            .metadata_index
            .iter()
            .find(|entry| entry.xml_path.ends_with("}intendedApplications"))
            .expect("actual metadata basis");
        assert_eq!(basis.text.as_deref(), Some("Global & exact"));
        let retained = inspected
            .projection
            .pointer(&basis.pointer)
            .expect("exact basis pointer");
        assert_eq!(basis.sha256, canonical_json_sha256(retained).unwrap());
        let projected = serde_json::to_string(&inspected.projection).unwrap();
        assert!(
            projected.contains("processing_instruction")
                && projected.contains("retain")
                && projected.contains("x:flag")
        );
    }

    #[test]
    fn factor_repetition_order_and_unknown_fields_change_hashes() {
        let a = factor("<location>A</location>");
        let b = factor("<location>B</location>");
        let ab = inspect_ilcd_method(method(&format!("{a}{b}")).as_bytes()).unwrap();
        let ba = inspect_ilcd_method(method(&format!("{b}{a}")).as_bytes()).unwrap();
        let aa = inspect_ilcd_method(method(&format!("{a}{a}")).as_bytes()).unwrap();
        assert_ne!(ab.ordered_factors_sha256, ba.ordered_factors_sha256);
        assert_ne!(ab.ordered_factors_sha256, aa.ordered_factors_sha256);
        let changed =
            inspect_ilcd_method(method(&a.replace("preserve", "changed")).as_bytes()).unwrap();
        let original = inspect_ilcd_method(method(&a).as_bytes()).unwrap();
        assert_ne!(
            changed.canonical_body_sha256,
            original.canonical_body_sha256
        );
        assert_ne!(
            changed.ordered_factors_sha256,
            original.ordered_factors_sha256
        );
    }

    #[test]
    fn rejects_entities_dtd_ambiguity_namespaces_and_malformed_xml() {
        let valid = method(&factor("<location/>"));
        for source in [
            valid.replace(
                "<?source keep?>",
                "<!DOCTYPE LCIAMethodDataSet SYSTEM 'file:///never-read'>",
            ),
            valid.replace(
                "<?source keep?>",
                "<!DOCTYPE LCIAMethodDataSet [<!ENTITY x 'replacement'>]>",
            ),
            valid.replace("Global &amp; exact", "&undeclared;"),
            valid.replace(
                "<location/>",
                "<location>A</location><location>B</location>",
            ),
            valid.replace("</factor>", "</wrong>"),
            valid.replace("xmlns:x=\"urn:unknown\"", ""),
            valid.replace(
                "<c:dataSetVersion>01.00.000</c:dataSetVersion>",
                "<c:dataSetVersion>unknown</c:dataSetVersion>",
            ),
            format!("{valid}<extra/>"),
        ] {
            assert!(
                inspect_ilcd_method(source.as_bytes()).is_err(),
                "must reject unsafe/ambiguous XML"
            );
        }
    }

    #[test]
    fn rejects_excessive_depth_and_preserves_cdata_without_trimming() {
        let deep = format!(
            "{}{}",
            "<x>".repeat(MAX_DEPTH + 1),
            "</x>".repeat(MAX_DEPTH + 1)
        );
        assert!(inspect_ilcd_method(deep.as_bytes()).is_err());
        let source = method(&factor("<location><![CDATA[ A ]]></location>"));
        let inspected = inspect_ilcd_method(source.as_bytes()).unwrap();
        assert_eq!(
            inspected.method_document["LCIAMethodDataSet"]["characterisationFactors"]["factor"][0]
                ["location"],
            " A "
        );
    }

    #[test]
    fn attribute_projection_preserves_raw_whitespace_and_xml_normalized_entities() {
        let raw = "left\tmiddle\r\nright &amp; &#x9; &#xA; &#xD;";
        let source = method(&factor("<location>A</location>"))
            .replace("x:flag=\"keep\"", &format!("x:flag=\"{raw}\""));
        let inspected = inspect_ilcd_method(source.as_bytes()).expect("valid XML 1.0 attribute");
        let root = inspected.projection["content"]
            .as_array()
            .unwrap()
            .iter()
            .find(|node| node["kind"] == "element")
            .unwrap();
        let attribute = root["attributes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|attribute| attribute["name"] == "x:flag")
            .unwrap();
        assert_eq!(attribute["raw_value"], raw);
        assert_eq!(attribute["value"], "left middle right & \t \n \r");
        assert_eq!(
            inspected.method_document["LCIAMethodDataSet"]["@x:flag"],
            "left middle right & \t \n \r"
        );
        assert_eq!(inspected.raw_sha256, sha256_bytes(source.as_bytes()));
    }
}
