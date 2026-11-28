//! `account_equity`: the broker account's equity at the close of one session. The newest row per
//! environment, strategy config and session is that session's equity.

use chrono::NaiveDate;
use nisse_core::StrategyVersionId;
use nisse_core::nautilus::UnixNanos;
use rust_decimal::Decimal;
use tokio_postgres::GenericClient;

use crate::{StoreError, db::to_ts};

/// Records the account's equity at the close of `session_date` for `version` in `environment`
/// (`paper` or `live`). A correction or a re-run is a new row; the newest row of a session wins.
/// An unknown environment or a negative equity is refused by the table's CHECK and comes back as
/// a `Db` error.
pub async fn insert<C: GenericClient>(
    client: &C,
    version: StrategyVersionId,
    environment: &str,
    session_date: NaiveDate,
    equity: Decimal,
    ts: UnixNanos,
) -> Result<(), StoreError> {
    let event_ts = to_ts(ts)?;
    client
        .execute(
            "INSERT INTO account_equity \
             (strategy_version_id, environment, session_date, equity, event_ts) \
             VALUES ($1, $2, $3, $4::numeric, $5)",
            &[&version.0, &environment, &session_date, &equity, &event_ts],
        )
        .await?;
    Ok(())
}

/// The equity series of `version`'s strategy config in `environment`: one `(session_date, equity)`
/// per session, ascending by date, taking the newest row (highest id) of each session. Rows of
/// every strategy version with the same `config_hash` as `version` are included, so a new build
/// of the same config continues the series. One statement.
pub async fn daily<C: GenericClient>(
    client: &C,
    version: StrategyVersionId,
    environment: &str,
) -> Result<Vec<(NaiveDate, Decimal)>, StoreError> {
    let rows = client
        .query(
            "SELECT DISTINCT ON (e.session_date) e.session_date, e.equity::numeric AS equity \
             FROM account_equity e \
             JOIN strategy_version v ON v.id = e.strategy_version_id \
             WHERE e.environment = $2 \
               AND v.config_hash = (SELECT config_hash FROM strategy_version WHERE id = $1) \
             ORDER BY e.session_date, e.id DESC",
            &[&version.0, &environment],
        )
        .await?;

    let mut series = Vec::with_capacity(rows.len());
    for row in rows {
        let session_date: NaiveDate = row.try_get("session_date")?;
        let equity: Decimal = row.try_get("equity")?;
        series.push((session_date, equity));
    }
    Ok(series)
}
