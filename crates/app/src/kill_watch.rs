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
    /// The store says the switch is engaged (or could not be read, which counts as engaged).
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

    let open = broker
        .open_orders()
        .await
        .map_err(|e| EngineError::Broker(Box::new(e)))?;
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

    match send_cancels(broker, &open, flatten_on_kill).await {
        Ok(cancelled) => {
            if cancelled {
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
        Err(err) => {
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
            Err(AppError::Engine(err))
        }
    }
}

/// Sends the cancels for `open` and says whether at least one was sent. With
/// `flatten_on_kill` only the buys are cancelled (the sells are the exits), each one by one,
/// trying every buy before returning the first error. Anything counted as unrepresentable
/// cannot be cancelled one by one, so with `flatten_on_kill` it falls back to cancelling
/// everything: something else is trading the account. Without it, everything is cancelled.
async fn send_cancels<B: Broker>(
    broker: &B,
    open: &OpenOrders,
    flatten_on_kill: bool,
) -> Result<bool, EngineError> {
    if !flatten_on_kill || open.unrepresentable > 0 {
        broker
            .cancel_all()
            .await
            .map_err(|e| EngineError::Broker(Box::new(e)))?;
        return Ok(true);
    }
    let mut sent = false;
    let mut first_error = None;
    for order in open.orders.iter().filter(|o| o.side == OrderSide::Buy) {
        match broker.cancel(order.venue_order_id).await {
            Ok(()) => sent = true,
            Err(err) => {
                if first_error.is_none() {
                    first_error = Some(err);
                }
            }
        }
    }
    match first_error {
        Some(err) => Err(EngineError::Broker(Box::new(err))),
        None => Ok(sent),
    }
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
