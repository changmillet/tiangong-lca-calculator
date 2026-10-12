//! LCIA display metadata shared by snapshot and frozen-source consumers.

use serde_json::Value;

const INFORMATION_PATHS: [(&str, &str); 4] = [
    ("LCIAMethodInformation", "dataSetInformation"),
    ("methodInformation", "dataSetInformation"),
    ("methodInfo", "dataSetInformation"),
    ("methodInfo", "dataSetInfo"),
];

/// Return the source localized name without discarding its language tags.
/// Canonical ILCD fields take precedence over the retained legacy aliases.
#[must_use]
pub fn name_value(document: &Value) -> Option<&Value> {
    let root = document.get("LCIAMethodDataSet")?;
    INFORMATION_PATHS
        .into_iter()
        .find_map(|(information, data_set_information)| {
            let data = root.get(information)?.get(data_set_information)?;
            [
                data.get("common:name"),
                data.get("name")
                    .map(|name| name.get("baseName").unwrap_or(name)),
            ]
            .into_iter()
            .flatten()
            .find(|value| localized_text(Some(value)).is_some())
        })
}

/// Select a nonempty English name, or the first readable localized alternative.
#[must_use]
pub fn name(document: &Value) -> Option<String> {
    localized_text(name_value(document))
}

/// Read the method's result-quantity label, not the referenced property's generic unit.
///
/// For example, both `kg CO2 equivalents` and `kg P equivalents` can refer to the
/// same Mass property and kilogram unit group. Resolving that chain to `kg` would
/// lose the indicator semantics. This matches the static LCIA list contract and
/// performs no factor conversion. Reference/source validation remains upstream.
#[must_use]
pub fn result_unit(document: &Value) -> Option<String> {
    let root = document.get("LCIAMethodDataSet")?;
    for (information, _) in INFORMATION_PATHS {
        let Some(reference) = root
            .get(information)
            .and_then(|info| info.get("quantitativeReference"))
        else {
            continue;
        };
        if let Some(quantity) = reference.get("referenceQuantity") {
            // An explicitly wrong reference type is not a usable quantity label.
            // Missing type remains readable for supported compact metadata forms.
            if quantity
                .get("@type")
                .is_some_and(|kind| kind.as_str() != Some("flow property data set"))
            {
                return None;
            }
            return localized_text(quantity.get("common:shortDescription"));
        }
        if let Some(unit_group) = reference.get("referenceToReferenceUnitGroup") {
            return localized_text(unit_group.get("common:shortDescription"));
        }
    }
    None
}

pub(crate) fn localized_text(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(value) => Some(value.trim().to_owned()).filter(|value| !value.is_empty()),
        Value::Object(value) => value
            .get("#text")
            .or_else(|| value.get("value"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned),
        Value::Array(values) => values
            .iter()
            .find(|value| {
                value
                    .get("@xml:lang")
                    .or_else(|| value.get("@lang"))
                    .and_then(Value::as_str)
                    .is_some_and(|language| language.eq_ignore_ascii_case("en"))
            })
            .and_then(|value| localized_text(Some(value)))
            .or_else(|| values.iter().find_map(|value| localized_text(Some(value)))),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{name, name_value, result_unit};

    #[test]
    fn canonical_names_preserve_localization_and_precede_legacy_aliases() {
        for text in [
            json!(" Climate change "),
            json!({"@xml:lang": "en", "#text": "Climate change"}),
            json!([
                {"@xml:lang": "zh", "#text": "气候变化"},
                {"@xml:lang": "EN", "#text": "Climate change"}
            ]),
        ] {
            let document = json!({"LCIAMethodDataSet": {
                "LCIAMethodInformation": {"dataSetInformation": {"common:name": text}},
                "methodInformation": {"dataSetInformation": {"name": {"baseName": "old"}}}
            }});
            assert_eq!(name(&document).as_deref(), Some("Climate change"));
            assert_eq!(name_value(&document), Some(&text));
        }
    }

    #[test]
    fn retained_information_aliases_read_names_and_unit_group_labels() {
        for (information, data_information) in super::INFORMATION_PATHS {
            let document = json!({"LCIAMethodDataSet": {information: {
                data_information: {"name": {"baseName": {"#text": "Legacy indicator"}}},
                "quantitativeReference": {"referenceToReferenceUnitGroup": {
                    "common:shortDescription": "legacy unit"
                }}
            }}});
            assert_eq!(name(&document).as_deref(), Some("Legacy indicator"));
            assert_eq!(result_unit(&document).as_deref(), Some("legacy unit"));
        }
    }

    #[test]
    fn unusable_canonical_name_allows_an_explicit_legacy_name() {
        let document = json!({"LCIAMethodDataSet": {
            "LCIAMethodInformation": {"dataSetInformation": {"common:name": " "}},
            "methodInfo": {"dataSetInfo": {"name": {"baseName": "Legacy indicator"}}}
        }});
        assert_eq!(name(&document).as_deref(), Some("Legacy indicator"));
    }

    #[test]
    fn quantity_labels_keep_equivalent_substance_despite_shared_physical_reference() {
        for label in ["kg CO2 equivalents", "kg P equivalents", "CTUh", "CTUe"] {
            let document = json!({"LCIAMethodDataSet": {"LCIAMethodInformation": {
                "quantitativeReference": {
                    "referenceQuantity": {
                        "@type": "flow property data set",
                        "@refObjectId": "93a60a56-a3c8-11da-a746-0800200b9a66",
                        "@version": "03.00.003",
                        "common:shortDescription": [
                            {"@xml:lang": "zh", "#text": "本地化单位"},
                            {"@xml:lang": "en", "#text": label}
                        ]
                    },
                    "referenceToReferenceUnitGroup": {"common:shortDescription": "kg"}
                }
            }}});
            assert_eq!(result_unit(&document).as_deref(), Some(label));
        }
    }

    #[test]
    fn missing_or_invalid_metadata_never_infers_a_label() {
        assert_eq!(name(&json!({})), None);
        assert_eq!(result_unit(&json!({})), None);
        for invalid in [
            json!(null),
            json!(42),
            json!(true),
            json!(" "),
            json!({}),
            json!([]),
        ] {
            let document = json!({"LCIAMethodDataSet": {"LCIAMethodInformation": {
                "dataSetInformation": {"common:name": invalid, "impactCategory": "Climate"},
                "quantitativeReference": {"referenceQuantity": {
                    "common:shortDescription": invalid
                }}
            }}});
            assert_eq!(name(&document), None);
            assert_eq!(result_unit(&document), None);
        }
        let wrong_type = json!({"LCIAMethodDataSet": {"LCIAMethodInformation": {
            "quantitativeReference": {"referenceQuantity": {
                "@type": "flow data set", "common:shortDescription": "not a quantity"
            }}
        }}});
        assert_eq!(result_unit(&wrong_type), None);
    }
}
