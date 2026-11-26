//! `order_event`: one row per state of an order. The first row for a client order id is the
//! write-ahead intent.

use chrono::{DateTime, Utc};
use nisse_core::nautilus::{
    ClientOrderId, OrderSide, OrderType, Symbol, TimeInForce, VenueOrderId,
};
use nisse_core::{DecisionId, OrderEvent, OrderIntent, OrderState, RestoredOrder};
use rust_decimal::{Decimal, RoundingStrategy};
use tokio_postgres::GenericClient;

use crate::StoreError;
use crate::db::{from_ts, parse_enum, to_price, to_quantity, to_ts};

/// Every `OrderState`. `index` below must match every variant, so adding a state breaks the
/// build until this list and the `CHECK` constraint in `migrations/` are updated.
const ALL: [OrderState; 10] = [
    OrderState::Intent,
    OrderState::Submitted,
    OrderState::Accepted,
    OrderState::PartiallyFilled,
    OrderState::PendingCancel,
    OrderState::Filled,
    OrderState::Canceled,
    OrderState::Replaced,
    OrderState::Rejected,
    OrderState::Expired,
];

/// Position of `state` in [`ALL`]. Exhaustive on purpose: a new variant fails to compile here.
const fn index(state: OrderState) -> usize {
    match state {
        OrderState::Intent => 0,
        OrderState::Submitted => 1,
        OrderState::Accepted => 2,
        OrderState::PartiallyFilled => 3,
        OrderState::PendingCancel => 4,
        OrderState::Filled => 5,
        OrderState::Canceled => 6,
        OrderState::Replaced => 7,
        OrderState::Rejected => 8,
        OrderState::Expired => 9,
    }
}

/// The stored strings of every terminal state, built from [`ALL`] so they cannot drift from
/// `OrderState::is_terminal`.
fn terminal_states() -> Vec<&'static str> {
    ALL.iter()
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

/// Every order whose latest row is not in a terminal state, in client order id order. An
/// order whose latest state is terminal is not returned.
///
/// One statement. The latest row is the highest `event_ts`, then the highest `id`; the intent
/// is the order's `state = 'intent'` row; the average fill price is the quantity-weighted mean
/// of its `fill` rows.
pub async fn load_open<C: GenericClient>(client: &C) -> Result<Vec<RestoredOrder>, StoreError> {
    let terminal = terminal_states();
    let rows = client
        .query(
            "WITH latest AS ( \
                 SELECT DISTINCT ON (client_order_id) client_order_id, broker_order_id, state, \
                        cum_filled_qty, event_ts \
                 FROM order_event ORDER BY client_order_id, event_ts DESC, id DESC \
             ), intent AS ( \
                 SELECT DISTINCT ON (client_order_id) * \
                 FROM order_event WHERE state = 'intent' ORDER BY client_order_id, event_ts, id \
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
             WHERE NOT (l.state = ANY($1)) \
             ORDER BY l.client_order_id",
            &[&terminal],
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
    fn all_lists_every_state_once_in_index_order() {
        for (position, state) in ALL.iter().enumerate() {
            assert_eq!(index(*state), position, "ALL is out of order at {state}");
        }
        let mut names: Vec<&str> = ALL.iter().map(|s| <&str>::from(*s)).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), ALL.len(), "ALL holds a duplicate state");
    }

    #[test]
    fn terminal_states_are_the_terminal_variants() {
        assert_eq!(
            terminal_states(),
            vec!["filled", "canceled", "replaced", "rejected", "expired"]
        );
    }
}
