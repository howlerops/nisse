//! Connecting, and the conversions every table module shares.
//!
//! Timestamps: the engine works in nanoseconds; Postgres `TIMESTAMPTZ` holds microseconds.
//! Values are truncated to the microsecond on the way in, so a timestamp read back can be up
//! to 999 ns earlier than the one written. Nothing in the store is keyed on exact time.
//!
//! Numbers: store columns use the `finite_numeric` / `finite_double` domains. Bind and read
//! them through their base types with a cast in the SQL (`$1::numeric`, `px::numeric`),
//! because the client only knows how to convert the base types.

use std::str::FromStr;

use chrono::{DateTime, Utc};
use nisse_core::nautilus::{Price, Quantity, UnixNanos};
use rust_decimal::Decimal;
use tokio_postgres::{Client, NoTls};

use crate::StoreError;

/// Opens a connection and spawns the task that drives it. The engine and its database share a
/// host, so the connection is not encrypted; do not point this at a remote database.
///
/// The URL is a libpq connection string (`postgres://user@host/db`). It may carry a password,
/// so it is never logged or put in an error.
pub async fn connect(url: &str) -> Result<Client, StoreError> {
    let (client, connection) = tokio_postgres::connect(url, NoTls).await?;
    tokio::spawn(async move {
        if let Err(error) = connection.await {
            tracing::error!(%error, "store connection ended");
        }
    });
    Ok(client)
}

/// Engine time to a `TIMESTAMPTZ` value (truncated to the microsecond).
pub(crate) fn to_ts(ts: UnixNanos) -> Result<DateTime<Utc>, StoreError> {
    i64::try_from(ts.as_u64() / 1_000)
        .ok()
        .and_then(DateTime::from_timestamp_micros)
        .ok_or_else(|| StoreError::Data(format!("timestamp out of range: {ts}")))
}

/// A `TIMESTAMPTZ` value to engine time.
pub(crate) fn from_ts(ts: DateTime<Utc>) -> Result<UnixNanos, StoreError> {
    ts.timestamp_nanos_opt()
        .and_then(|nanos| u64::try_from(nanos).ok())
        .map(UnixNanos::from)
        .ok_or_else(|| StoreError::Data(format!("timestamp out of range: {ts}")))
}

/// A stored `NUMERIC` to a Nautilus quantity (non-negative, at most 9 decimal places).
pub(crate) fn to_quantity(value: Decimal) -> Result<Quantity, StoreError> {
    Quantity::from_str(&value.to_string())
        .map_err(|e| StoreError::Data(format!("bad quantity {value}: {e}")))
}

/// A stored `NUMERIC` to a Nautilus price.
pub(crate) fn to_price(value: Decimal) -> Result<Price, StoreError> {
    Price::from_str(&value.to_string())
        .map_err(|e| StoreError::Data(format!("bad price {value}: {e}")))
}

/// Parses a stored enum string with the type's `FromStr`, naming the column on failure.
pub(crate) fn parse_enum<T: FromStr>(text: &str, column: &'static str) -> Result<T, StoreError> {
    text.parse()
        .map_err(|_| StoreError::Data(format!("unknown {column} value: {text}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_truncate_to_microseconds_and_round_trip() {
        let written = UnixNanos::from(1_650_390_305_024_916_716);
        let stored = to_ts(written).unwrap();
        assert_eq!(stored.timestamp_micros(), 1_650_390_305_024_916);
        assert_eq!(
            from_ts(stored).unwrap(),
            UnixNanos::from(1_650_390_305_024_916_000)
        );
    }

    #[test]
    fn decimals_convert_to_fixed_point_exactly() {
        let qty = to_quantity("1.5".parse().unwrap()).unwrap();
        assert_eq!(qty.as_decimal().to_string(), "1.5");
        assert!(to_quantity("-1".parse().unwrap()).is_err());
        let px = to_price("500.10".parse().unwrap()).unwrap();
        assert_eq!(px.as_decimal(), "500.1".parse::<Decimal>().unwrap());
    }

    #[test]
    fn enums_parse_or_name_the_column() {
        let state: nisse_core::OrderState = parse_enum("partially_filled", "state").unwrap();
        assert_eq!(state, nisse_core::OrderState::PartiallyFilled);
        let err = parse_enum::<nisse_core::OrderState>("bogus", "state").unwrap_err();
        assert!(err.to_string().contains("state"));
    }
}
