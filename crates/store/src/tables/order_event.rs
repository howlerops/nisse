//! `order_event`: one row per state of an order. The first row for a client order id is the
//! write-ahead intent.

use chrono::{DateTime, Utc};
use nisse_core::nautilus::{
    ClientOrderId, OrderSide, OrderType, Symbol, TimeInForce, UnixNanos, VenueOrderId,
};
use nisse_core::{DecisionId, OrderEvent, OrderIntent, OrderState, RestoredOrder};
use rust_decimal::{Decimal, RoundingStrategy};
use strum::VariantArray;
use tokio_postgres::GenericClient;

use crate::StoreError;
use crate::db::{from_ts, parse_enum, to_price, to_quantity, to_ts};

/// The stored strings of every terminal state, from the enum itself so they cannot drift
/// from `OrderState::is_terminal`.
pub(crate) fn terminal_states() -> Vec<&'static str> {
    OrderState::VARIANTS
        .iter()
        .filter(|state| state.is_terminal())
        .map(|state| <&'static str>::from(*state))
        .collect()
}

/// Inserts one row. This is the write-ahead intent when `event` is `OrderEvent::intent`, and it
/// is awaited before the order is sent, so it is exactly one statement.
pub async fn insert<C: GenericClient>(client: &C, event: &OrderEvent) -> Result<(), StoreError> {
    let event_ts = to_ts(event.ts)?;
    let limit_px = event.limit_px.map(|px| px.as_decimal());
    client
        .execute(
            "INSERT INTO order_event (decision_id, client_order_id, broker_order_id, state, type, \
             limit_px, qty, symbol, side, time_in_force, cum_filled_qty, event_ts) \
             VALUES ($1, $2, $3, $4, $5, $6::numeric, $7::numeric, $8, $9, $10, $11::numeric, $12)",
            &[
                &event.decision_id.0,
                &event.client_order_id.as_str(),
                &event.broker_order_id.as_ref().map(VenueOrderId::as_str),
                &<&'static str>::from(event.state),
                &event.order_type.to_string(),
                &limit_px,
                &event.qty.as_decimal(),
                &event.symbol.as_str(),
                &event.side.to_string(),
                &event.time_in_force.to_string(),
                &event.cum_filled_qty.as_decimal(),
                &event_ts,
            ],
        )
        .await?;
    Ok(())
}

/// Every order whose latest row is not in a terminal state, in client order id order.
/// Equivalent to [`load_live`] with no recent-terminal window.
pub async fn load_open<C: GenericClient>(client: &C) -> Result<Vec<RestoredOrder>, StoreError> {
    load_live(client, None).await
}

/// The orders a restarting engine must track, in client order id order: every order whose
/// latest row is not terminal, plus, when `ended_since` is given, every order whose latest
/// row is terminal but was stored at or after that time. The second group matters because a
/// terminal order can still fill (a fill racing a cancel), and because a "never arrived"
/// verdict is provisional: without them a late fill would arrive for an order the engine no
/// longer knows.
///
/// One statement. "Latest" is the row inserted last (highest `id`), not the highest
/// `event_ts`: event times come from two clocks (the engine's for intents, the broker's for
/// its updates), and a small skew must not make an intent look newer than the fill after it.
/// `ended_since` is compared with `ingest_ts`, which only the database sets.
pub async fn load_live<C: GenericClient>(
    client: &C,
    ended_since: Option<UnixNanos>,
) -> Result<Vec<RestoredOrder>, StoreError> {
    let ended_since = ended_since.map(to_ts).transpose()?;
    let terminal = terminal_states();
    let rows = client
        .query(
            "WITH latest AS ( \
                 SELECT DISTINCT ON (client_order_id) client_order_id, broker_order_id, state, \
                        cum_filled_qty, event_ts, ingest_ts \
                 FROM order_event ORDER BY client_order_id, id DESC \
             ), intent AS ( \
                 SELECT DISTINCT ON (client_order_id) * \
                 FROM order_event WHERE state = 'intent' ORDER BY client_order_id, id \
             ), fills AS ( \
                 SELECT client_order_id, sum(px * qty) / sum(qty) AS avg_px \
                 FROM fill GROUP BY client_order_id \
             ) \
             SELECT i.decision_id, i.client_order_id, i.symbol, i.side, i.type, \
                    i.qty::numeric AS qty, i.limit_px::numeric AS limit_px, i.time_in_force, \
                    i.event_ts AS intent_ts, \
                    l.broker_order_id, l.state, l.cum_filled_qty::numeric AS cum_filled_qty, \
                    l.event_ts AS last_ts, f.avg_px::numeric AS avg_px \
             FROM latest l \
             JOIN intent i USING (client_order_id) \
             LEFT JOIN fills f USING (client_order_id) \
             WHERE NOT (l.state = ANY($1)) OR COALESCE(l.ingest_ts >= $2, false) \
             ORDER BY l.client_order_id",
            &[&terminal, &ended_since],
        )
        .await?;

    let mut orders = Vec::with_capacity(rows.len());
    for row in rows {
        let client_order_id: String = row.try_get("client_order_id")?;
        let symbol: String = row.try_get("symbol")?;
        let side: String = row.try_get("side")?;
        let order_type: String = row.try_get("type")?;
        let time_in_force: String = row.try_get("time_in_force")?;
        let qty: Decimal = row.try_get("qty")?;
        let limit_px: Option<Decimal> = row.try_get("limit_px")?;
        let intent_ts: DateTime<Utc> = row.try_get("intent_ts")?;
        let broker_order_id: Option<String> = row.try_get("broker_order_id")?;
        let state: String = row.try_get("state")?;
        let cum_filled_qty: Decimal = row.try_get("cum_filled_qty")?;
        let last_ts: DateTime<Utc> = row.try_get("last_ts")?;
        let avg_px: Option<Decimal> = row.try_get("avg_px")?;

        let intent = OrderIntent {
            decision_id: DecisionId(row.try_get("decision_id")?),
            client_order_id: parse_id(
                ClientOrderId::new_checked(&client_order_id),
                "client order id",
            )?,
            symbol: parse_id(Symbol::new_checked(&symbol), "symbol")?,
            side: parse_enum::<OrderSide>(&side, "side")?,
            order_type: parse_enum::<OrderType>(&order_type, "type")?,
            qty: to_quantity(qty)?,
            limit_px: limit_px.map(to_price).transpose()?,
            time_in_force: parse_enum::<TimeInForce>(&time_in_force, "time_in_force")?,
            ts: from_ts(intent_ts)?,
        };
        let avg_fill_px = avg_px
            .map(|px| {
                let rounded = px
                    .round_dp_with_strategy(9, RoundingStrategy::MidpointNearestEven)
                    .normalize();
                to_price(rounded)
            })
            .transpose()?;

        orders.push(RestoredOrder {
            intent,
            venue_order_id: broker_order_id
                .as_deref()
                .map(|id| parse_id(VenueOrderId::new_checked(id), "broker order id"))
                .transpose()?,
            state: parse_enum::<OrderState>(&state, "state")?,
            cum_filled_qty: to_quantity(cum_filled_qty)?,
            avg_fill_px,
            last_update_ts: from_ts(last_ts)?,
        });
    }
    Ok(orders)
}

/// Rebuilds an identifier from its stored text.
fn parse_id<T, E: std::fmt::Display>(
    checked: Result<T, E>,
    what: &'static str,
) -> Result<T, StoreError> {
    checked.map_err(|e| StoreError::Data(format!("bad stored {what}: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_states_are_the_terminal_variants() {
        assert_eq!(
            terminal_states(),
            vec!["filled", "canceled", "replaced", "rejected", "expired"]
        );
    }
}
