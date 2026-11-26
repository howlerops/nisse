//! Parsing helpers shared by every Alpaca payload: timestamps and exact decimals.
//!
//! Alpaca sends money and quantities either as JSON strings (`"150.25"`, trading API) or as
//! JSON numbers (`178.21`, market data API). Both are read from the original text, never
//! through `f64`, so no value is rounded on the way in.

use std::str::FromStr;

use chrono::DateTime;
use nisse_core::nautilus::{Price, Quantity, UnixNanos};
use serde_json::value::RawValue;

use crate::AdapterError;

/// Parses an RFC 3339 timestamp (any offset, up to nanosecond precision) to UTC nanoseconds.
pub(crate) fn parse_ts(text: &str) -> Result<UnixNanos, AdapterError> {
    let nanos = DateTime::parse_from_rfc3339(text)
        .ok()
        .and_then(|ts| ts.timestamp_nanos_opt())
        .and_then(|nanos| u64::try_from(nanos).ok())
        .ok_or_else(|| AdapterError::Decode(format!("bad timestamp: {text}")))?;
    Ok(UnixNanos::from(nanos))
}

/// Whether `text` is a plain decimal: an optional `-`, digits, at most one `.`. No exponent,
/// no NaN, no empty string. Nautilus' own parsers accept `1e5`, which Alpaca never sends and
/// which this adapter refuses rather than interpret.
fn is_plain_decimal(text: &str) -> bool {
    let digits = text.strip_prefix('-').unwrap_or(text);
    digits.bytes().all(|b| b.is_ascii_digit() || b == b'.')
        && digits.bytes().filter(|b| *b == b'.').count() <= 1
        && digits.bytes().any(|b| b.is_ascii_digit())
}

/// The decimal text of a JSON string or number, without quotes. Rejects anything that is not
/// a plain decimal (`null`, exponents, NaN), so callers can hand it to a fixed-point parser.
pub(crate) fn decimal_text(raw: &RawValue) -> Result<&str, AdapterError> {
    let text = raw.get().trim_matches('"');
    if is_plain_decimal(text) {
        Ok(text)
    } else {
        Err(AdapterError::Decode(format!(
            "not a decimal: {}",
            raw.get()
        )))
    }
}

/// Parses plain decimal text to a [`Price`], keeping the precision it was written with.
pub(crate) fn parse_price(text: &str) -> Result<Price, AdapterError> {
    if !is_plain_decimal(text) {
        return Err(AdapterError::Decode(format!("bad price {text}")));
    }
    Price::from_str(text).map_err(|e| AdapterError::Decode(format!("bad price {text}: {e}")))
}

/// Parses plain decimal text to a [`Quantity`] (non-negative), keeping its precision.
pub(crate) fn parse_qty(text: &str) -> Result<Quantity, AdapterError> {
    if !is_plain_decimal(text) {
        return Err(AdapterError::Decode(format!("bad quantity {text}")));
    }
    Quantity::from_str(text).map_err(|e| AdapterError::Decode(format!("bad quantity {text}: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(json: &str) -> Box<RawValue> {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn timestamps_with_offsets_and_nanos_become_utc_nanos() {
        assert_eq!(
            parse_ts("2022-04-19T17:45:05.024916716Z").unwrap().as_u64(),
            1_650_390_305_024_916_716
        );
        // Same instant written with a -04:00 offset.
        assert_eq!(
            parse_ts("2022-04-19T13:45:05.024916716-04:00").unwrap(),
            parse_ts("2022-04-19T17:45:05.024916716Z").unwrap()
        );
        assert!(parse_ts("2022-04-19").is_err());
        assert!(parse_ts("1969-12-31T23:59:59Z").is_err());
    }

    #[test]
    fn decimal_text_reads_strings_and_numbers_exactly() {
        assert_eq!(decimal_text(&raw("\"150.25\"")).unwrap(), "150.25");
        assert_eq!(decimal_text(&raw("178.21")).unwrap(), "178.21");
        assert_eq!(decimal_text(&raw("1118")).unwrap(), "1118");
        assert_eq!(decimal_text(&raw("\"-23140.2\"")).unwrap(), "-23140.2");
        for bad in [
            "null",
            "\"\"",
            "1e5",
            "\"NaN\"",
            "\"1.2.3\"",
            "\".\"",
            "true",
        ] {
            assert!(decimal_text(&raw(bad)).is_err(), "{bad} must be rejected");
        }
    }

    #[test]
    fn prices_and_quantities_keep_their_precision_and_value() {
        let px = parse_price("105.8988475").unwrap();
        assert_eq!(px.to_string(), "105.8988475");
        assert_eq!(parse_price("0.1").unwrap().as_decimal().to_string(), "0.1");
        let qty = parse_qty("1790.86").unwrap();
        assert_eq!(qty.to_string(), "1790.86");
        assert!(parse_qty("-1").is_err());
        assert!(parse_price("abc").is_err());
        for exponent in ["1e5", "1E-2", ""] {
            assert!(parse_price(exponent).is_err(), "{exponent}");
            assert!(parse_qty(exponent).is_err(), "{exponent}");
        }
    }
}
