//! Exact finite-f64 JSON decoding scoped to Study. Legacy JSON parsing is unchanged.
//! `RawValue` preserves lexical tokens; Rust's f64 parser supplies roundtrip precision.
use anyhow::{Context, ensure};
use serde::{
    Deserializer,
    de::{DeserializeOwned, Error, MapAccess, SeqAccess, Visitor},
};
use serde_json::{Number, Value, value::RawValue};
use std::fmt;

pub const MAX_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_NODES: usize = 262_144;
pub const MAX_DEPTH: usize = 128;
struct Budget {
    nodes: usize,
}
pub fn from_slice<T: DeserializeOwned>(bytes: &[u8]) -> anyhow::Result<T> {
    ensure!(bytes.len() <= MAX_BYTES, "study_json_byte_limit");
    let raw: &RawValue = serde_json::from_slice(bytes)?;
    Ok(serde_json::from_value(value(
        raw,
        0,
        &mut Budget { nodes: 0 },
    )?)?)
}
pub fn from_str<T: DeserializeOwned>(text: &str) -> anyhow::Result<T> {
    from_slice(text.as_bytes())
}
fn value(raw: &RawValue, depth: usize, budget: &mut Budget) -> anyhow::Result<Value> {
    ensure!(depth < MAX_DEPTH, "study_json_depth_limit");
    ensure!(budget.nodes < MAX_NODES, "study_json_node_limit");
    budget.nodes += 1;
    let text = raw.get();
    match text.as_bytes().first().copied() {
        Some(b'{') => Ok(Value::Object(
            serde_json::Deserializer::from_str(text).deserialize_map(Object { depth, budget })?,
        )),
        Some(b'[') => Ok(Value::Array(
            serde_json::Deserializer::from_str(text).deserialize_seq(Array { depth, budget })?,
        )),
        Some(b'"') => Ok(Value::String(serde_json::from_str(text)?)),
        Some(b't' | b'f') => Ok(Value::Bool(serde_json::from_str(text)?)),
        Some(b'n') => Ok(Value::Null),
        Some(_) => {
            if !text.contains(['.', 'e', 'E']) && text != "-0" {
                if let Ok(number) = text.parse::<i64>() {
                    return Ok(Value::Number(number.into()));
                }
                if let Ok(number) = text.parse::<u64>() {
                    return Ok(Value::Number(number.into()));
                }
            }
            let number = text.parse::<f64>()?;
            let significand = text.split(['e', 'E']).next().unwrap_or("");
            ensure!(
                number != 0.0 || !significand.bytes().any(|c| matches!(c, b'1'..=b'9')),
                "study_json_number_underflow"
            );
            Ok(Value::Number(
                Number::from_f64(number).context("study_json_number_overflow")?,
            ))
        }
        None => anyhow::bail!("study_json_empty"),
    }
}
struct Array<'a> {
    depth: usize,
    budget: &'a mut Budget,
}
impl<'de> Visitor<'de> for Array<'_> {
    type Value = Vec<Value>;
    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("bounded Study JSON array")
    }
    fn visit_seq<S: SeqAccess<'de>>(self, mut sequence: S) -> Result<Self::Value, S::Error> {
        // Stream borrowed tokens; never allocate an unbounded vector of raw child tokens first.
        let mut result = Vec::new();
        while let Some(raw) = sequence.next_element::<&'de RawValue>()? {
            result.push(value(raw, self.depth + 1, self.budget).map_err(S::Error::custom)?);
        }
        Ok(result)
    }
}
struct Object<'a> {
    depth: usize,
    budget: &'a mut Budget,
}
impl<'de> Visitor<'de> for Object<'_> {
    type Value = serde_json::Map<String, Value>;
    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("bounded Study JSON object")
    }
    fn visit_map<M: MapAccess<'de>>(self, mut fields: M) -> Result<Self::Value, M::Error> {
        let mut result = serde_json::Map::new();
        while let Some((key, raw)) = fields.next_entry::<String, &'de RawValue>()? {
            if key.starts_with("$serde_json::private::") {
                return Err(M::Error::custom("study_json_reserved_key"));
            }
            if result.contains_key(&key) {
                return Err(M::Error::custom("study_json_duplicate_key"));
            }
            result.insert(
                key,
                value(raw, self.depth + 1, self.budget).map_err(M::Error::custom)?,
            );
        }
        Ok(result)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn finite_values_retain_bits_and_nonfinite_range_fails() {
        for bits in [
            1,
            0x3f3e_8db8_bf10_42e3,
            0x3fd6_e0eb_edfa_43fe,
            0x7fef_ffff_ffff_ffff,
        ] {
            let expected = f64::from_bits(bits);
            let bytes = serde_json::to_vec(&expected).unwrap();
            let actual: f64 = from_slice(&bytes).unwrap();
            assert_eq!(actual.to_bits(), expected.to_bits());
        }
        assert!(from_str::<Value>("1e400").is_err());
        assert!(from_str::<Value>("1e-999").is_err());
        assert_eq!(
            from_str::<f64>("-0.0").unwrap().to_bits(),
            (-0.0_f64).to_bits()
        );
    }
    #[test]
    fn integers_allocations_and_structure_bounds_are_preserved() {
        let values:Value=from_str("{\"id\":18446744073709551615,\"negative\":-9223372036854775808,\"allocation\":{\"value\":0.35748000000000004}}").unwrap();
        assert_eq!(values["id"].as_u64(), Some(u64::MAX));
        assert_eq!(values["negative"].as_i64(), Some(i64::MIN));
        assert_eq!(
            values["allocation"]["value"].as_f64().unwrap().to_bits(),
            "0.35748000000000004".parse::<f64>().unwrap().to_bits()
        );
        assert!(from_str::<Value>("{\"x\":1,\"x\":2}").is_err());
        assert!(from_str::<Value>(r#"{"$serde_json::private::RawValue":"[[[[0]]]]"}"#).is_err());
        assert!(
            from_str::<Value>(&format!(
                "{}0{}",
                "[".repeat(MAX_DEPTH + 1),
                "]".repeat(MAX_DEPTH + 1)
            ))
            .is_err()
        );
        assert!(from_str::<Value>(&format!("[{}0]", "0,".repeat(MAX_NODES))).is_err());
        assert!(from_slice::<Value>(&vec![b' '; MAX_BYTES + 1]).is_err());
        assert!(from_str::<Value>("[1,]").is_err());
        assert!(from_str::<Value>("{invalid}").is_err());
    }
}
