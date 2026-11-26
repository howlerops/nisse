//! Reads that span several tables, for the scoring job and the gate report. Like every
//! module here: `SELECT` only, and each function takes `&impl GenericClient`.

use std::str::FromStr;

use chrono::{DateTime, Utc};
use nisse_core::nautilus::{OrderSide, Symbol, UnixNanos};
use nisse_core::{DecisionId, StrategyVersionId};
use rust_decimal::Decimal;
use serde_json::Value;
use tokio_postgres::GenericClient;

use crate::StoreError;
use crate::db::{from_ts, parse_enum};

/// One fill with the decision, symbol and side of its order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecisionFill {
    pub decision_id: DecisionId,
    pub symbol: Symbol,
    pub side: OrderSide,
    pub qty: Decimal,
    pub px: Decimal,
    pub fee: Decimal,
    pub ts: UnixNanos,
}

/// What a decision's orders asked for and whether they are all finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecisionOrders {
    pub decision_id: DecisionId,
    /// Taken from the decision's first order (lowest `order_event` id). Orders of one
    /// decision are expected to agree; this read does not check that.
    pub symbol: Symbol,
    /// Taken from the decision's first order, as for `symbol`.
    pub side: OrderSide,
    /// Sum of `qty` over the decision's orders (each order counted once, from its first row).
    pub ordered_qty: Decimal,
    /// Every order's newest row (by id) is in a terminal state.
    pub all_terminal: bool,
    /// At least one order has a row beyond the intent.
    pub sent: bool,
}

/// One backtest trial with a stored result.
#[derive(Debug, Clone, PartialEq)]
pub struct TrialWithResult {
    pub trial_id: i64,
    pub strategy_version_id: StrategyVersionId,
    /// `trial_registry.result @> '{"holdout": true}'`.
    pub holdout: bool,
    pub result: Value,
    /// The trial's own timestamp (`trial_registry.event_ts`), not the result's.
    pub ts: UnixNanos,
}

/// Every fill, in `fill.id` order, with its order's decision, symbol and side (from the
/// order's first `order_event` row by id). A fill whose order has no `order_event` row is a
/// [`StoreError::Data`]: a fill that cannot be attributed to a decision is never skipped.
///
/// One statement; the lateral lookup uses `order_event_client_order_id_event_ts_idx`.
pub async fn fills<C: GenericClient>(client: &C) -> Result<Vec<DecisionFill>, StoreError> {
    let rows = client
        .query(
            "SELECT f.id, o.decision_id, o.symbol, o.side, f.qty::numeric AS qty, \
                    f.px::numeric AS px, f.fee::numeric AS fee, f.event_ts \
             FROM fill f \
             LEFT JOIN LATERAL ( \
                 SELECT decision_id, symbol, side FROM order_event \
                 WHERE client_order_id = f.client_order_id ORDER BY id LIMIT 1 \
             ) o ON true \
             ORDER BY f.id",
            &[],
        )
        .await?;

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let id: i64 = row.try_get("id")?;
        let decision_id: Option<i64> = row.try_get("decision_id")?;
        let symbol: Option<String> = row.try_get("symbol")?;
        let side: Option<String> = row.try_get("side")?;
        let (Some(decision_id), Some(symbol), Some(side)) = (decision_id, symbol, side) else {
            return Err(StoreError::Data(format!(
                "fill {id} has no order_event row for its client order id"
            )));
        };
        let event_ts: DateTime<Utc> = row.try_get("event_ts")?;
        out.push(DecisionFill {
            decision_id: DecisionId(decision_id),
            symbol: parse_symbol(&symbol)?,
            side: parse_enum::<OrderSide>(&side, "side")?,
            qty: row.try_get("qty")?,
            px: row.try_get("px")?,
            fee: row.try_get("fee")?,
            ts: from_ts(event_ts)?,
        });
    }
    Ok(out)
}

/// One entry per decision that has at least one `order_event` row, in decision id order.
/// The order-level figures come from each order's first row (`ordered_qty`, symbol, side) and
/// newest row (`all_terminal`); `sent` is true when any row is beyond the intent.
///
/// One statement; each CTE scans `order_event` once.
pub async fn decision_orders<C: GenericClient>(
    client: &C,
) -> Result<Vec<DecisionOrders>, StoreError> {
    let terminal = super::order_event::terminal_states();
    let rows = client
        .query(
            "WITH first_rows AS ( \
                 SELECT DISTINCT ON (client_order_id) id, decision_id, client_order_id, symbol, \
                        side, qty \
                 FROM order_event ORDER BY client_order_id, id \
             ), newest AS ( \
                 SELECT DISTINCT ON (client_order_id) client_order_id, state \
                 FROM order_event ORDER BY client_order_id, id DESC \
             ), beyond_intent AS ( \
                 SELECT client_order_id, bool_or(state <> 'intent') AS sent \
                 FROM order_event GROUP BY client_order_id \
             ) \
             SELECT f.decision_id, \
                    (array_agg(f.symbol ORDER BY f.id))[1] AS symbol, \
                    (array_agg(f.side ORDER BY f.id))[1] AS side, \
                    sum(f.qty)::numeric AS ordered_qty, \
                    bool_and(n.state = ANY($1)) AS all_terminal, \
                    bool_or(b.sent) AS sent \
             FROM first_rows f \
             JOIN newest n USING (client_order_id) \
             JOIN beyond_intent b USING (client_order_id) \
             GROUP BY f.decision_id \
             ORDER BY f.decision_id",
            &[&terminal],
        )
        .await?;

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let symbol: String = row.try_get("symbol")?;
        let side: String = row.try_get("side")?;
        let decision_id: i64 = row.try_get("decision_id")?;
        out.push(DecisionOrders {
            decision_id: DecisionId(decision_id),
            symbol: parse_symbol(&symbol)?,
            side: parse_enum::<OrderSide>(&side, "side")?,
            ordered_qty: row.try_get("ordered_qty")?,
            all_terminal: row.try_get("all_terminal")?,
            sent: row.try_get("sent")?,
        });
    }
    Ok(out)
}

/// The decision's `close` feature (`decision.features ->> 'close'`), the price it was made at,
/// for each decision id given, in id order. A decision without that feature, or whose
/// feature is not a decimal, is left out. An empty slice returns nothing without a query.
///
/// One statement for the whole slice.
pub async fn arrival_prices<C: GenericClient>(
    client: &C,
    decision_ids: &[DecisionId],
) -> Result<Vec<(DecisionId, Decimal)>, StoreError> {
    if decision_ids.is_empty() {
        return Ok(Vec::new());
    }
    let ids: Vec<i64> = decision_ids.iter().map(|id| id.0).collect();
    let rows = client
        .query(
            "SELECT id, features ->> 'close' AS close FROM decision \
             WHERE id = ANY($1) AND features ->> 'close' IS NOT NULL \
             ORDER BY id",
            &[&ids],
        )
        .await?;

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let id: i64 = row.try_get("id")?;
        let close: String = row.try_get("close")?;
        // A feature that is not a decimal is left out, not an error: it carries no price.
        if let Ok(price) = Decimal::from_str(&close) {
            out.push((DecisionId(id), price));
        }
    }
    Ok(out)
}

/// The dollar risk the gate recorded for each decision's order: `risk_check.actual_value` of
/// the newest row (by id) with `rule = 'risk_per_trade'` for that decision, when it is
/// positive. A decision whose newest such value is 0 or negative is left out. In decision
/// id order.
///
/// One statement; `DISTINCT ON` picks the newest row per decision before the sign filter.
pub async fn entry_risks<C: GenericClient>(
    client: &C,
) -> Result<Vec<(DecisionId, Decimal)>, StoreError> {
    let rows = client
        .query(
            "SELECT decision_id, actual FROM ( \
                 SELECT DISTINCT ON (decision_id) decision_id, actual_value::numeric AS actual \
                 FROM risk_check WHERE rule = 'risk_per_trade' \
                 ORDER BY decision_id, id DESC \
             ) newest \
             WHERE actual > 0 \
             ORDER BY decision_id",
            &[],
        )
        .await?;

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let decision_id: i64 = row.try_get("decision_id")?;
        out.push((DecisionId(decision_id), row.try_get("actual")?));
    }
    Ok(out)
}

/// Every trial of kind `backtest` that has a `trial_result` row, in trial id order. A trial
/// without a result (still running, or crashed) is not returned.
///
/// One statement.
pub async fn backtest_results<C: GenericClient>(
    client: &C,
) -> Result<Vec<TrialWithResult>, StoreError> {
    let rows = client
        .query(
            "SELECT t.id, t.strategy_version_id, \
                    COALESCE(t.result @> '{\"holdout\": true}', false) AS holdout, \
                    r.result, t.event_ts \
             FROM trial_registry t \
             JOIN trial_result r ON r.trial_id = t.id \
             WHERE t.kind = 'backtest' \
             ORDER BY t.id",
            &[],
        )
        .await?;

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let event_ts: DateTime<Utc> = row.try_get("event_ts")?;
        let version: i64 = row.try_get("strategy_version_id")?;
        out.push(TrialWithResult {
            trial_id: row.try_get("id")?,
            strategy_version_id: StrategyVersionId(version),
            holdout: row.try_get("holdout")?,
            result: row.try_get("result")?,
            ts: from_ts(event_ts)?,
        });
    }
    Ok(out)
}

/// Decisions of `version` whose NEWEST `replay_match` score (by `eval_score.id`) is 0, as
/// `(failed, scored)`: `scored` is the number of the version's decisions with any such
/// score. Other metrics are ignored; older scores are superseded, not averaged.
///
/// One statement.
pub async fn replay_failures<C: GenericClient>(
    client: &C,
    version: StrategyVersionId,
) -> Result<(i64, i64), StoreError> {
    let row = client
        .query_one(
            "WITH latest AS ( \
                 SELECT DISTINCT ON (e.subject_id) e.subject_id, e.value \
                 FROM eval_score e \
                 JOIN decision d ON d.id = e.subject_id \
                 WHERE d.strategy_version_id = $1 \
                   AND e.subject_type = 'decision' AND e.metric = 'replay_match' \
                 ORDER BY e.subject_id, e.id DESC \
             ) \
             SELECT count(*) FILTER (WHERE value = 0) AS failed, count(*) AS scored \
             FROM latest",
            &[&version.0],
        )
        .await?;
    Ok((row.try_get("failed")?, row.try_get("scored")?))
}

/// Orders of `version`'s decisions that went beyond the intent (have a row whose state is not
/// `intent`) although one of the decision's `risk_check` rows has `passed = false`. Counted
/// once per `client_order_id`.
///
/// One statement.
pub async fn risk_breaches<C: GenericClient>(
    client: &C,
    version: StrategyVersionId,
) -> Result<i64, StoreError> {
    let row = client
        .query_one(
            "SELECT count(DISTINCT o.client_order_id) \
             FROM order_event o \
             JOIN decision d ON d.id = o.decision_id \
             WHERE d.strategy_version_id = $1 \
               AND o.state <> 'intent' \
               AND EXISTS ( \
                   SELECT 1 FROM risk_check r \
                   WHERE r.decision_id = o.decision_id AND NOT r.passed \
               )",
            &[&version.0],
        )
        .await?;
    Ok(row.try_get(0)?)
}

/// Rebuilds a symbol from its stored text.
fn parse_symbol(text: &str) -> Result<Symbol, StoreError> {
    Symbol::new_checked(text).map_err(|e| StoreError::Data(format!("bad stored symbol: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
}
