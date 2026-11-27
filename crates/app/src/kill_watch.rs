//! Enforces the kill switch from the store and the broker alone: no trader, no bar files, no
//! deployment signature and no engine lock. The `kill-watch` command runs this every minute,
//! and `tick` and `session-close` run it first, so an engaged switch (a row written from the
//! phone or any other way) cancels what rests at the broker at any hour.

use std::path::Path;

use nisse_core::nautilus::{OrderSide, UnixNanos};
use nisse_core::{Broker, KillSwitchSource, OpenOrders};
use nisse_engine::EngineError;
use nisse_risk::Limits;
use nisse_store::{Store, tables::engine_step::StepStatus};
use serde_json::json;

use crate::AppError;

/// The run key and step under which a cancel made because of the kill switch is recorded.
pub const KILL_RUN_KEY: &str = "kill";
pub const KILL_CANCEL_STEP: &str = "cancel";

/// What enforcing the kill switch found and did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KillEnforcement {
    /// The store says the switch is engaged. A newest switch row that cannot be read counts as
    /// engaged; a store that cannot be reached at all is an error, and nothing is cancelled
    /// (a database outage must not cancel orders every minute).
    pub engaged: bool,
    /// Orders the broker had open when it was asked (0 when the switch is off: the broker is not asked).
    pub open_orders: usize,
    /// A cancel was sent.
    pub cancelled: bool,
}

/// Enforces the kill switch from the store and the broker alone: no trader, no bar files, no
/// signature, no engine lock. Safe to run at any time, alongside any other engine command.
pub async fn enforce_kill<B: Broker>(
    store: &Store,
    broker: &B,
    flatten_on_kill: bool,
    now: UnixNanos,
) -> Result<KillEnforcement, AppError> {
    let engaged = match store.kill_switch_engaged().await {
        Ok(engaged) => engaged,
        Err(err) => {
            tracing::error!(error = %err, "kill switch unreadable; treating it as engaged");
            true
        }
    };
    if !engaged {
        return Ok(KillEnforcement {
            engaged: false,
            open_orders: 0,
            cancelled: false,
        });
    }

    let open = match broker.open_orders().await {
        Ok(open) => open,
        Err(err) => {
            let err = EngineError::Broker(Box::new(err));
            return listing_failed(store, broker, flatten_on_kill, err, now).await;
        }
    };
    let open_orders = open
        .orders
        .len()
        .saturating_add(usize::try_from(open.unrepresentable).unwrap_or(usize::MAX));
    if open_orders == 0 {
        return Ok(KillEnforcement {
            engaged: true,
            open_orders: 0,
            cancelled: false,
        });
    }

    let (cancelled, error) = send_cancels(broker, &open, flatten_on_kill).await;
    if let Some(err) = error {
        record_failure(store, &err, now).await;
        // A switch that halts must have cancelled everything: anything less fails the run.
        // One that flattens has more to do after this (the rebalance cancels buys itself and
        // sends the exits), so a buy that could not be cancelled, perhaps because it filled a
        // moment ago, is recorded and the run goes on.
        if !flatten_on_kill {
            return Err(AppError::Engine(err));
        }
    } else if cancelled {
        store
            .record_step(
                KILL_RUN_KEY,
                KILL_CANCEL_STEP,
                StepStatus::Done,
                &json!({"open_orders": open_orders, "flatten": flatten_on_kill}),
                now,
            )
            .await?;
    }
    Ok(KillEnforcement {
        engaged: true,
        open_orders,
        cancelled,
    })
}

/// The broker could not list its open orders while the switch is engaged. A switch that
/// halts does not need the list: everything is cancelled blind, and that is recorded. One
/// that flattens must not cancel the exits, so it cannot act without the list: the failure
/// is recorded and returned.
async fn listing_failed<B: Broker>(
    store: &Store,
    broker: &B,
    flatten_on_kill: bool,
    listing_error: EngineError,
    now: UnixNanos,
) -> Result<KillEnforcement, AppError> {
    if flatten_on_kill {
        record_failure(store, &listing_error, now).await;
        return Err(AppError::Engine(listing_error));
    }
    if let Err(err) = broker.cancel_all().await {
        let err = EngineError::Broker(Box::new(err));
        record_failure(store, &err, now).await;
        return Err(AppError::Engine(err));
    }
    store
        .record_step(
            KILL_RUN_KEY,
            KILL_CANCEL_STEP,
            StepStatus::Done,
            &json!({"open_orders": null, "flatten": false, "listing_failed": listing_error.to_string()}),
            now,
        )
        .await?;
    Ok(KillEnforcement {
        engaged: true,
        open_orders: 0,
        cancelled: true,
    })
}

/// Records that enforcing the switch failed, so it can be seen from the phone. If the record
/// itself cannot be written, that is logged: the caller still has the error.
async fn record_failure(store: &Store, err: &impl std::fmt::Display, now: UnixNanos) {
    let body = json!({"error": err.to_string()});
    if let Err(record_err) = store
        .record_step(
            KILL_RUN_KEY,
            KILL_CANCEL_STEP,
            StepStatus::Failed,
            &body,
            now,
        )
        .await
    {
        tracing::error!(
            step = KILL_CANCEL_STEP,
            error = %record_err,
            "could not record the failed kill cancel"
        );
    }
}

/// The heartbeat source the watcher writes under after every run that did its job.
pub const WATCH_SOURCE: &str = "kill-watch";

/// One run of the watcher: enforce the switch, then write the watcher's own heartbeat, so
/// that a watcher that is not running (or cannot do its job) can be told from one with
/// nothing to do. `broker` is `None` when no broker client could be built (missing keys):
/// that only matters when the switch is engaged, and then it is a recorded failure.
pub async fn watch_once<B: Broker>(
    store: &Store,
    broker: Option<&B>,
    flatten_on_kill: bool,
    now: UnixNanos,
) -> Result<KillEnforcement, AppError> {
    let outcome = match broker {
        Some(broker) => enforce_kill(store, broker, flatten_on_kill, now).await?,
        None => {
            let engaged = match store.kill_switch_engaged().await {
                Ok(engaged) => engaged,
                Err(err) => {
                    tracing::error!(error = %err, "kill switch unreadable; treating it as engaged");
                    true
                }
            };
            if engaged {
                let message = "the kill switch is engaged but there is no broker client: \
                               open orders were NOT cancelled";
                record_failure(store, &message, now).await;
                return Err(AppError::KillNotEnforced(message.to_owned()));
            }
            KillEnforcement {
                engaged: false,
                open_orders: 0,
                cancelled: false,
            }
        }
    };
    store.heartbeat(WATCH_SOURCE, now).await?;
    Ok(outcome)
}

/// Sends the cancels for `open` and says whether at least one was sent, and what failed. With
/// `flatten_on_kill` only the buys are cancelled (the sells are the exits), each one by one,
/// trying every buy; the first error, if any, is returned beside what was sent. Anything counted as unrepresentable
/// cannot be cancelled one by one, so with `flatten_on_kill` it falls back to cancelling
/// everything: something else is trading the account. Without it, everything is cancelled.
async fn send_cancels<B: Broker>(
    broker: &B,
    open: &OpenOrders,
    flatten_on_kill: bool,
) -> (bool, Option<EngineError>) {
    if !flatten_on_kill || open.unrepresentable > 0 {
        return match broker.cancel_all().await {
            Ok(()) => (true, None),
            Err(err) => (false, Some(EngineError::Broker(Box::new(err)))),
        };
    }
    let mut sent = false;
    let mut first_error = None;
    for order in open.orders.iter().filter(|o| o.side == OrderSide::Buy) {
        match broker.cancel(order.venue_order_id).await {
            Ok(()) => sent = true,
            Err(err) => {
                if first_error.is_none() {
                    first_error = Some(EngineError::Broker(Box::new(err)));
                }
            }
        }
    }
    (sent, first_error)
}

/// Whether the limits at `path` say a kill flattens. A limits file that cannot be read counts
/// as "no": the switch then halts and everything is cancelled, which is the safe reading.
pub fn flatten_on_kill(limits_path: &Path) -> bool {
    match Limits::load(limits_path) {
        Ok(limits) => limits.flatten_on_kill,
        Err(err) => {
            tracing::error!(error = %err, "limits unreadable; a kill halts and cancels everything");
            false
        }
    }
}
