//! Enforces the kill switch from the store and the broker alone: no trader, no bar files, no
//! deployment signature and no engine lock. The `kill-watch` command runs this every minute,
//! and `tick` and `session-close` run it first, so an engaged switch (a row written from the
//! phone or any other way) cancels what rests at the broker at any hour.

use std::path::Path;

use nisse_core::nautilus::{OrderSide, UnixNanos};
use nisse_core::{Broker, KillSwitchSource, OpenOrders};
use nisse_engine::EngineError;
use nisse_risk::Limits;
use nisse_store::{Store, StoreError, tables::engine_step::StepStatus};
use serde_json::json;

use crate::AppError;

/// The run key and step under which a cancel made because of the kill switch is recorded.
pub const KILL_RUN_KEY: &str = "kill";
pub const KILL_CANCEL_STEP: &str = "cancel";

/// What enforcing the kill switch found and did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KillEnforcement {
    /// The store says the switch is engaged. A newest switch row that cannot be understood
    /// counts as engaged; any other failure to read the switch is an error, and nothing is
    /// cancelled (a database outage must not cancel orders every minute).
    pub engaged: bool,
    /// Orders the broker had open when it was asked (0 when the switch is off, when the
    /// broker is not asked, and when it could not say).
    pub open_orders: usize,
    /// A cancel was sent.
    pub cancelled: bool,
    /// A cancel was refused and the run went on (only with `flatten_on_kill`; without it a
    /// refused cancel is an error). The failure is recorded as a step.
    pub failed: bool,
}

impl KillEnforcement {
    const OFF: Self = Self {
        engaged: false,
        open_orders: 0,
        cancelled: false,
        failed: false,
    };
}

/// Reads the switch. `Ok(true)` also for a newest switch row that cannot be understood
/// (someone wrote a kill row this build cannot read: fail closed). Any other error, such as a
/// lost connection, is returned: it says nothing about the switch.
async fn switch_engaged(store: &Store) -> Result<bool, AppError> {
    match store.kill_switch_engaged().await {
        Ok(engaged) => Ok(engaged),
        Err(StoreError::Data(_)) => Ok(true),
        Err(err) => Err(err.into()),
    }
}

/// How many of `open` an engaged switch wants gone: every order when the switch halts (or
/// when something the engine cannot represent is open), the buys when it flattens.
fn targets(open: &OpenOrders, flatten_on_kill: bool) -> usize {
    let unrepresentable = usize::try_from(open.unrepresentable).unwrap_or(usize::MAX);
    if !flatten_on_kill || unrepresentable > 0 {
        open.orders.len().saturating_add(unrepresentable)
    } else {
        open.orders
            .iter()
            .filter(|order| order.side == OrderSide::Buy)
            .count()
    }
}

/// Enforces the kill switch from the store and the broker alone: no trader, no bar files, no
/// signature, no engine lock. Safe to run at any time, alongside any other engine command.
///
/// What it records under `kill` / `cancel`, which the phone shows: `started` when cancels
/// were sent but the broker still lists orders that should be gone (or could not be asked);
/// `done` only when a listing made after the cancel shows none left; `failed` with the error
/// when a cancel was refused or could not be attempted.
pub async fn enforce_kill<B: Broker>(
    store: &Store,
    broker: &B,
    flatten_on_kill: bool,
    now: UnixNanos,
) -> Result<KillEnforcement, AppError> {
    if !switch_engaged(store).await? {
        return Ok(KillEnforcement::OFF);
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
    if targets(&open, flatten_on_kill) == 0 {
        // Nothing left to cancel. If the last cancel was sent but never confirmed, this
        // listing is the confirmation.
        if matches!(
            store.step_status(KILL_RUN_KEY, KILL_CANCEL_STEP).await?,
            Some((StepStatus::Started, _))
        ) {
            record(
                store,
                StepStatus::Done,
                json!({"confirmed_empty": true}),
                now,
            )
            .await?;
        }
        return Ok(KillEnforcement {
            engaged: true,
            open_orders,
            cancelled: false,
            failed: false,
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
        return Ok(KillEnforcement {
            engaged: true,
            open_orders,
            cancelled,
            failed: true,
        });
    }

    // The broker accepting a cancel is not the orders being gone (it answers before it has
    // cancelled, and may fail order by order): look again, and say "done" only for an empty
    // list.
    let remaining = match broker.open_orders().await {
        Ok(after) => Some(targets(&after, flatten_on_kill)),
        Err(_) => None,
    };
    let detail =
        json!({"open_orders": open_orders, "flatten": flatten_on_kill, "remaining": remaining});
    let status = if remaining == Some(0) {
        StepStatus::Done
    } else {
        StepStatus::Started
    };
    record(store, status, detail, now).await?;
    Ok(KillEnforcement {
        engaged: true,
        open_orders,
        cancelled,
        failed: false,
    })
}

/// The broker could not list its open orders while the switch is engaged. A switch that
/// halts does not need the list: everything is cancelled blind, recorded as `started` because
/// nothing can confirm it. One that flattens must not cancel the exits, so it cannot act
/// without the list: the failure is recorded and returned.
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
    record(
        store,
        StepStatus::Started,
        json!({"flatten": false, "remaining": null, "listing_failed": listing_error.to_string()}),
        now,
    )
    .await?;
    Ok(KillEnforcement {
        engaged: true,
        open_orders: 0,
        cancelled: true,
        failed: false,
    })
}

/// Appends one `kill` / `cancel` row.
async fn record(
    store: &Store,
    status: StepStatus,
    detail: serde_json::Value,
    now: UnixNanos,
) -> Result<(), AppError> {
    store
        .record_step(KILL_RUN_KEY, KILL_CANCEL_STEP, status, &detail, now)
        .await?;
    Ok(())
}

/// Records that enforcing the switch failed, so it can be seen from the phone. If the record
/// itself cannot be written, that goes to standard error (the engine installs no log
/// subscriber): the caller still has the error.
async fn record_failure(store: &Store, err: &impl std::fmt::Display, now: UnixNanos) {
    if let Err(record_err) = record(
        store,
        StepStatus::Failed,
        json!({"error": err.to_string()}),
        now,
    )
    .await
    {
        eprintln!("nisse-engine: could not record the failed kill cancel: {record_err}");
    }
}

/// The heartbeat source the watcher writes under after every run that could have cancelled.
pub const WATCH_SOURCE: &str = "kill-watch";

/// One run of the watcher: enforce the switch, then write the watcher's own heartbeat.
///
/// The heartbeat means "a kill engaged now would have been enforced": the store was read,
/// the broker answered an authenticated request, and no cancel was refused. So `broker` being
/// `Err` (no client could be built: the reason is given), a broker that does not answer, and
/// a refused cancel all end without a heartbeat, and the phone then shows the watcher as not
/// running, which for its purpose it is not.
pub async fn watch_once<B: Broker>(
    store: &Store,
    broker: Result<&B, String>,
    flatten_on_kill: bool,
    now: UnixNanos,
) -> Result<KillEnforcement, AppError> {
    let broker = match broker {
        Ok(broker) => broker,
        Err(reason) => {
            let message = format!("no broker client: {reason}");
            if switch_engaged(store).await? {
                record_failure(store, &message, now).await;
            }
            return Err(AppError::KillNotEnforced(message));
        }
    };
    let outcome = enforce_kill(store, broker, flatten_on_kill, now).await?;
    if outcome.failed {
        return Err(AppError::KillNotEnforced(
            "a cancel was refused; see the kill/cancel step".to_owned(),
        ));
    }
    if !outcome.engaged {
        // The switch is off, so the broker was not needed. Ask it something anyway: a
        // watcher whose keys no longer work must not look healthy.
        broker
            .account()
            .await
            .map_err(|err| EngineError::Broker(Box::new(err)))?;
    }
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
            eprintln!(
                "nisse-engine: limits unreadable ({err}); a kill halts and cancels everything"
            );
            false
        }
    }
}
