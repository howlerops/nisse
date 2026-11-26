//! Canonical text forms and their hashes.
//!
//! A decision is replayed from its features, its strategy's parameters and a seed, and the
//! store keeps a hash of each so tampering or drift is detectable. A hash is only stable if
//! the bytes it covers are: same key order, same number formatting, no whitespace. This
//! module defines those bytes once.

use rust_decimal::Decimal;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::str::FromStr;

use crate::{CoreError, Hash32};

/// The inputs a strategy decides from: named exact decimals. A `BTreeMap`, so iteration (and
/// therefore hashing and serialisation) is in name order.
pub type Features = BTreeMap<String, Decimal>;

/// JSON with object keys sorted (byte order) at every level and no insignificant whitespace.
/// Strings are escaped as `serde_json` escapes them; numbers are written as `serde_json`
/// writes them. Arrays keep their order.
#[must_use]
pub fn canonical_json(value: &Value) -> String {
    let mut out = String::with_capacity(256);
    write_canonical(&mut out, value);
    out
}

/// Appends the canonical form of `value` to `out`.
fn write_canonical(out: &mut String, value: &Value) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(n) => out.push_str(&n.to_string()),
        Value::String(s) => push_json_string(out, s),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(out, item);
            }
            out.push(']');
        }
        Value::Object(map) => {
            // Sort explicitly: `serde_json::Map` order depends on the `preserve_order` feature.
            let mut entries: Vec<(&String, &Value)> = map.iter().collect();
            entries.sort_unstable_by(|a, b| a.0.cmp(b.0));
            out.push('{');
            for (i, (key, item)) in entries.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                push_json_string(out, key);
                out.push(':');
                write_canonical(out, item);
            }
            out.push('}');
        }
    }
}

/// Appends `s` as a JSON string literal, escaped by `serde_json`.
fn push_json_string(out: &mut String, s: &str) {
    out.push_str(&serde_json::to_string(s).unwrap_or_default());
}

/// SHA-256 of the given bytes as a [`Hash32`].
fn sha256_hash(bytes: &[u8]) -> Hash32 {
    let digest: [u8; 32] = Sha256::digest(bytes).into();
    Hash32::from_bytes(digest)
}

/// SHA-256 of [`canonical_json`] of a strategy's parameters (`strategy_version.config_hash`).
#[must_use]
pub fn config_hash(params: &Value) -> Hash32 {
    sha256_hash(canonical_json(params).as_bytes())
}

/// Features as a JSON object whose values are decimal **strings** (`{"mom_12m":"0.1"}`), so
/// no value passes through a float on the way into or out of the store. Each decimal is
/// written normalised (no trailing zeros; `-0` is `0`).
#[must_use]
pub fn features_to_json(features: &Features) -> Value {
    let map: Map<String, Value> = features
        .iter()
        .map(|(name, value)| (name.clone(), Value::String(normalised_text(value))))
        .collect();
    Value::Object(map)
}

/// The normalised decimal text of `value`, with any zero written as `0`.
fn normalised_text(value: &Decimal) -> String {
    let normalised = value.normalize();
    if normalised.is_zero() {
        "0".to_owned()
    } else {
        normalised.to_string()
    }
}

/// The inverse of [`features_to_json`]. Errors with [`CoreError::InvalidFeatures`] unless the
/// value is an object whose every value is a string holding a plain decimal.
pub fn features_from_json(value: &Value) -> Result<Features, CoreError> {
    let Value::Object(map) = value else {
        return Err(CoreError::InvalidFeatures(
            "expected a JSON object".to_owned(),
        ));
    };
    let mut features = Features::new();
    for (name, raw) in map {
        let Value::String(text) = raw else {
            return Err(invalid_feature(name, "value is not a string"));
        };
        if !is_plain_decimal(text) {
            return Err(invalid_feature(name, "value is not a plain decimal"));
        }
        let decimal =
            Decimal::from_str(text).map_err(|_| invalid_feature(name, "value is out of range"))?;
        features.insert(name.clone(), decimal);
    }
    Ok(features)
}

/// A feature error naming the offending key.
fn invalid_feature(name: &str, reason: &str) -> CoreError {
    CoreError::InvalidFeatures(format!("feature {name:?}: {reason}"))
}

/// `-?[0-9]*\.?[0-9]*` with at least one digit. No exponent, sign other than a leading `-`,
/// or whitespace.
fn is_plain_decimal(text: &str) -> bool {
    let digits = text.strip_prefix('-').unwrap_or(text);
    let mut seen_dot = false;
    let mut seen_digit = false;
    for byte in digits.bytes() {
        match byte {
            b'0'..=b'9' => seen_digit = true,
            b'.' if !seen_dot => seen_dot = true,
            _ => return false,
        }
    }
    seen_digit
}

/// SHA-256 of `canonical_json(features_to_json(features))` (`decision.feature_hash`). Two
/// feature sets with equal values hash equally even if the decimals were written differently
/// (`1.50` and `1.5`).
#[must_use]
pub fn feature_hash(features: &Features) -> Hash32 {
    sha256_hash(canonical_json(&features_to_json(features)).as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse(text: &str) -> Value {
        serde_json::from_str(text).unwrap()
    }

    fn features(pairs: &[(&str, &str)]) -> Features {
        pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), Decimal::from_str(value).unwrap()))
            .collect()
    }

    fn hex(hash: Hash32) -> String {
        hash.to_string()
    }

    // canonical_json

    #[test]
    fn nested_objects_are_sorted_at_every_level() {
        let value = parse(
            r#"{"z":{"b":1,"a":{"y":true,"x":null}},"a":[{"d":"x","c":2},3],"m":{"q":{"t":{"s":0}}}}"#,
        );
        assert_eq!(
            canonical_json(&value),
            r#"{"a":[{"c":2,"d":"x"},3],"m":{"q":{"t":{"s":0}}},"z":{"a":{"x":null,"y":true},"b":1}}"#
        );
    }

    #[test]
    fn arrays_keep_their_order() {
        let value = parse(r#"{"k":[3,1,2,[ "b", "a" ]]}"#);
        assert_eq!(canonical_json(&value), r#"{"k":[3,1,2,["b","a"]]}"#);
    }

    #[test]
    fn whitespace_is_removed() {
        let value = parse(
            "{\n  \"b\" : [ 1 , 2 ,\t3 ],\n \"a\" : { \"y\" : \"x y\" , \"x\" : true } }\r\n",
        );
        // The space inside "x y" is string content and must survive; all others must go.
        assert_eq!(
            canonical_json(&value),
            r#"{"a":{"x":true,"y":"x y"},"b":[1,2,3]}"#
        );
    }

    #[test]
    fn strings_are_escaped_as_serde_json_escapes_them() {
        let input = "say \"hi\"\\ é\u{1}\n";
        let value = Value::String(input.to_owned());
        let expected = serde_json::to_string(input).unwrap();
        assert_eq!(canonical_json(&value), expected);
        assert_eq!(expected, "\"say \\\"hi\\\"\\\\ é\\u0001\\n\"");
    }

    #[test]
    fn object_keys_are_escaped_too() {
        let value = parse(r#"{"b\"q":1,"a\\":2}"#);
        assert_eq!(canonical_json(&value), r#"{"a\\":2,"b\"q":1}"#);
    }

    #[test]
    fn numbers_and_scalars_are_written_as_serde_json_writes_them() {
        let value = json!([1, -2, 3.5, null, true, false, "", {}, []]);
        assert_eq!(
            canonical_json(&value),
            r#"[1,-2,3.5,null,true,false,"",{},[]]"#
        );
    }

    #[test]
    fn key_order_does_not_change_the_text() {
        let a = parse(r#"{"a":1,"b":{"c":2,"d":3}}"#);
        let b = parse(r#"{"b":{"d":3,"c":2},"a":1}"#);
        assert_eq!(canonical_json(&a), canonical_json(&b));
        assert_eq!(canonical_json(&b), r#"{"a":1,"b":{"c":2,"d":3}}"#);
    }

    // config_hash

    #[test]
    fn config_hash_of_small_object_is_sha256_of_its_bytes() {
        assert_eq!(
            hex(config_hash(&json!({"a": 1}))),
            "015abd7f5cc57a2dd94b7590f04ad8084273905ee33ec5cebeae62276a97f862"
        );
    }

    #[test]
    fn config_hash_ignores_key_order() {
        let a = parse(r#"{"a":1,"b":2}"#);
        let b = parse(r#"{ "b": 2, "a": 1 }"#);
        assert_eq!(config_hash(&a), config_hash(&b));
    }

    #[test]
    fn config_hash_changes_with_a_value() {
        assert_ne!(config_hash(&json!({"a": 1})), config_hash(&json!({"a": 2})));
    }

    // features_to_json

    #[test]
    fn feature_values_are_json_strings() {
        let json = features_to_json(&features(&[("a", "0.1"), ("b", "-3")]));
        let Value::Object(map) = &json else {
            panic!("expected object");
        };
        assert_eq!(map.len(), 2);
        assert!(map.values().all(Value::is_string));
        assert_eq!(json, json!({"a": "0.1", "b": "-3"}));
    }

    #[test]
    fn equal_decimals_are_written_identically() {
        let json = features_to_json(&features(&[("x", "1.50"), ("y", "1.5")]));
        assert_eq!(json, json!({"x": "1.5", "y": "1.5"}));
    }

    #[test]
    fn negative_zero_is_written_as_zero() {
        let json = features_to_json(&features(&[("z", "-0"), ("w", "-0.000")]));
        assert_eq!(json, json!({"w": "0", "z": "0"}));
        assert_eq!(canonical_json(&json), r#"{"w":"0","z":"0"}"#);
    }

    #[test]
    fn trailing_zeros_are_removed() {
        let json = features_to_json(&features(&[("t", "100.00")]));
        assert_eq!(json, json!({"t": "100"}));
    }

    // features_from_json

    #[test]
    fn features_round_trip_as_normalised() {
        let original = features(&[
            ("a", "1.50"),
            ("b", "-0"),
            ("c", "12345678901234567890.123456789"),
            ("d", "0.000000001"),
        ]);
        let expected = features(&[
            ("a", "1.5"),
            ("b", "0"),
            ("c", "12345678901234567890.123456789"),
            ("d", "0.000000001"),
        ]);
        assert_eq!(
            features_from_json(&features_to_json(&original)),
            Ok(expected)
        );
    }

    #[test]
    fn large_and_tiny_decimals_survive_exactly() {
        let json = json!({"big": "12345678901234567890.123456789", "tiny": "0.000000001"});
        let parsed = features_from_json(&json).unwrap();
        assert_eq!(parsed["big"].to_string(), "12345678901234567890.123456789");
        assert_eq!(parsed["tiny"].to_string(), "0.000000001");
        assert_eq!(features_to_json(&parsed), json);
    }

    #[test]
    fn empty_object_is_empty_features() {
        assert_eq!(features_from_json(&json!({})), Ok(Features::new()));
    }

    #[test]
    fn rejects_non_objects() {
        let expected = Err(CoreError::InvalidFeatures(
            "expected a JSON object".to_owned(),
        ));
        assert_eq!(features_from_json(&json!([])), expected);
        assert_eq!(features_from_json(&json!("0.1")), expected);
        assert_eq!(features_from_json(&json!(null)), expected);
        assert_eq!(features_from_json(&json!(1)), expected);
    }

    #[test]
    fn rejects_number_values_and_names_the_key() {
        assert_eq!(
            features_from_json(&json!({"mom": 0.1})),
            Err(CoreError::InvalidFeatures(
                "feature \"mom\": value is not a string".to_owned()
            ))
        );
        assert_eq!(
            features_from_json(&json!({"ok": "1", "bad": null})),
            Err(CoreError::InvalidFeatures(
                "feature \"bad\": value is not a string".to_owned()
            ))
        );
    }

    #[test]
    fn rejects_non_decimal_strings() {
        for text in [
            "abc", "1e5", "", "+1", " 1", "1 ", "-", ".", "-.", "1.2.3", "--1", "NaN", "1_000",
            "0x10", "١",
        ] {
            assert_eq!(
                features_from_json(&json!({"k": text})),
                Err(CoreError::InvalidFeatures(
                    "feature \"k\": value is not a plain decimal".to_owned()
                )),
                "input {text:?}"
            );
        }
    }

    #[test]
    fn accepts_plain_decimal_shapes() {
        for text in ["0", "-0", "7", "-7.25", "0.5", ".5", "5.", "-.5"] {
            assert!(
                features_from_json(&json!({"k": text})).is_ok(),
                "input {text:?}"
            );
        }
    }

    #[test]
    fn rejects_out_of_range_decimals() {
        let text = "1".repeat(40);
        assert_eq!(
            features_from_json(&json!({"huge": text})),
            Err(CoreError::InvalidFeatures(
                "feature \"huge\": value is out of range".to_owned()
            ))
        );
    }

    // feature_hash

    #[test]
    fn feature_hash_ignores_decimal_spelling() {
        assert_eq!(
            feature_hash(&features(&[("a", "1.50")])),
            feature_hash(&features(&[("a", "1.5")]))
        );
    }

    #[test]
    fn feature_hash_changes_with_a_value_or_a_name() {
        let base = feature_hash(&features(&[("a", "1.5")]));
        assert_ne!(base, feature_hash(&features(&[("a", "1.6")])));
        assert_ne!(base, feature_hash(&features(&[("b", "1.5")])));
    }

    #[test]
    fn feature_hash_ignores_insertion_order() {
        let mut forward = Features::new();
        forward.insert("a".to_owned(), Decimal::from_str("1").unwrap());
        forward.insert("b".to_owned(), Decimal::from_str("2").unwrap());
        let mut backward = Features::new();
        backward.insert("b".to_owned(), Decimal::from_str("2").unwrap());
        backward.insert("a".to_owned(), Decimal::from_str("1").unwrap());
        assert_eq!(feature_hash(&forward), feature_hash(&backward));
    }

    #[test]
    fn empty_features_hash_to_sha256_of_empty_object() {
        assert_eq!(
            hex(feature_hash(&Features::new())),
            "44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a"
        );
    }

    #[test]
    fn feature_hash_is_sha256_of_the_canonical_text() {
        assert_eq!(
            hex(feature_hash(&features(&[("a", "1.50")]))),
            "87d1686a3b72d474dd6a634812da8bd415587b9d0411af8c2399acd06b59ad61"
        );
    }
}
