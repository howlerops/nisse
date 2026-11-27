//! The `tick` and `session-close` commands.
//!
//! `tick` runs every few minutes while the market is open: it reconciles with the broker,
//! observes the account equity and rebalances only when a drawdown step-down or a flatten is
//! owed. `session-close` runs once after the close: it ingests the session's bars, reconciles,
//! observes, and rebalances at a month end or when a change is owed. Both are idempotent: a
//! re-run skips what already completed. The steps are in `docs/phases/phase-6.md`.

use std::future::Future;

use chrono::{DateTime, Days, NaiveDate};
use nisse_core::nautilus::UnixNanos;
use nisse_core::{Broker, Clock, MarketCalendar, Session};
use nisse_engine::{EngineError, RebalanceReport, SyncReport};
use nisse_risk::DrawdownMode;
use nisse_store::{Store, StoreWriter, tables::engine_step::StepStatus};
use rust_decimal::Decimal;
use serde_json::{Value, json};

use crate::deployment::verify_from_env;
use crate::ingest::{IngestOutcome, ingest_session};
use crate::kill_watch::{enforce_kill, flatten_on_kill};
use crate::market::Market;
use crate::restore::{EngineTrader, Restored, restore, take_engine_lock};
use crate::{AppError, config::EngineConfig};

/// How long after a session's close `session-close` still decides for it.
pub const MAX_CLOSE_AGE_NANOS: u64 = 6 * 3_600 * 1_000_000_000;
/// The run key of the owed resize. A resize is owed while the newest status of
/// [`RESIZE_STEP`] under this key is `started`.
pub const RESIZE_RUN_KEY: &str = "resize";
/// The step recording an owed rebalance (a drawdown resize, or a rebalance that could not
/// be completed).
pub const RESIZE_STEP: &str = "pending";
/// The step under which a tick records its attempt at an owed rebalance. The run key is
/// `tick/<UTC date>T<hour>`: a completed attempt is not repeated within the hour unless the
/// risk scale has dropped since (a step-down, a flattening kill switch), so orders that stay
/// blocked do not turn into a set of decisions every tick.
pub const TICK_ATTEMPT_STEP: &str = "rebalance";

/// The step names of a `session-close` run.
const INGEST_STEP: &str = "ingest";
const RECONCILE_STEP: &str = "reconcile";
const OBSERVE_STEP: &str = "observe";
const REBALANCE_STEP: &str = "rebalance";

/// Nanoseconds in a second.
const NANOS_PER_SECOND: u64 = 1_000_000_000;
/// Days either side of today that the calendar is read for.
const CALENDAR_MARGIN_DAYS: u64 = 10;

/// What one `tick` did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TickOutcome {
    /// Fills the reconciliation pass found.
    pub fills: usize,
    /// The drawdown mode change this run observed, if any.
    pub mode_change: Option<(DrawdownMode, DrawdownMode)>,
    pub market_open: bool,
    /// The rebalance this tick ran: one was owed, the market was open, orders could go out,
    /// and no tick had completed an attempt in this hour at this risk scale or a lower one.
    pub rebalance: Option<RebalanceReport>,
    /// Open orders were cancelled because the switch was found engaged.
    pub kill_cancelled: bool,
}

/// What the rebalance step of a `session-close` run did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RebalanceStep {
    /// No rebalance was owed; the step is recorded `done` with `"due": false`.
    NotDue,
    /// The step was already `done` for this session (an earlier run): nothing was decided now.
    AlreadyDone,
    /// A rebalance was due but no order could go out (the kill switch without
    /// `flatten_on_kill`, or the account is blocked from trading). Nothing was decided; the
    /// rebalance is owed.
    Halted,
    /// The rebalance ran in this call. If any order was blocked or refused, it is still owed.
    Ran(RebalanceReport),
}

/// What one `session-close` call did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionCloseOutcome {
    /// No session closed within the last `MAX_CLOSE_AGE_NANOS`. Nothing was written.
    NoSession,
    Ran {
        session_date: NaiveDate,
        ingest: IngestOutcome,
        fills: usize,
        mode_change: Option<(DrawdownMode, DrawdownMode)>,
        rebalance: RebalanceStep,
    },
}

/// Runs one `tick`. Order: enforce the kill switch → verify the signature → lock → restore →
/// reconcile → observe → (re-read the switch) → rebalance if the market is open and one is
/// owed → finish. The kill switch is enforced first, from the store and the broker alone, so
/// it holds whatever the deployment or the engine's state. Any error returns before the
/// heartbeat.
pub async fn tick<M: Market, B: Broker, C: Clock>(
    config: &EngineConfig,
    market: &M,
    store: &Store,
    sink: StoreWriter,
    broker: B,
    clock: C,
) -> Result<TickOutcome, AppError> {
    let kill = enforce_kill(
        store,
        &broker,
        flatten_on_kill(&config.limits_path),
        clock.now(),
    )
    .await?;
    // Before the lock: a deployment that must be signed and is not trades nothing.
    verify_from_env(config)?;
    let now = clock.now();
    take_engine_lock(store).await?;
    let Restored { mut trader, .. } = restore(config, store, sink.clone(), broker, clock).await?;

    let fills = reconcile_pass(&mut trader).await?.fills.len();
    let observed = observe_pass(&mut trader, store, now).await?;

    let today = utc_date(now)?;
    let calendar = market
        .calendar(minus_margin(today)?, plus_margin(today)?)
        .await?;
    let market_open = calendar.is_open(now);

    let due = resize_owed(store).await? || trader.needs_flatten();
    // Only a rebalance about to be considered re-reads the switch: the restore read it before
    // this run's observe pass, and a row written since then must stop the rebalance.
    let may_rebalance = due
        && market_open
        && !halted(&trader, &observed)
        && !refresh_halts(&mut trader, store, &observed).await;
    let attempt_key = format!("tick/{}T{:02}", today, utc_hour(now));
    // An attempt that completed (whatever its orders met) is not repeated within the hour,
    // while one that failed part-way may be. The exception is a risk scale that has dropped
    // since that attempt (a drawdown step-down, a kill switch that flattens): that is acted
    // on at once. An attempt whose recorded scale cannot be read counts as tried.
    // Read after the switch is re-read: an engaged switch can change the scale.
    let scale = trader.risk().risk_scale();
    let tried_this_hour = match store.step_status(&attempt_key, TICK_ATTEMPT_STEP).await? {
        Some((StepStatus::Done, detail)) => detail
            .get("risk_scale")
            .and_then(Value::as_str)
            .and_then(|text| text.parse::<Decimal>().ok())
            .is_none_or(|tried_at| scale >= tried_at),
        _ => false,
    };
    let rebalance = if may_rebalance && !tried_this_hour {
        let report = step(
            store,
            &attempt_key,
            TICK_ATTEMPT_STEP,
            now,
            rebalance_pass(&mut trader),
            |report| {
                let mut detail = rebalance_detail(report);
                detail["risk_scale"] = json!(scale.to_string());
                detail
            },
        )
        .await?;
        settle_or_owe(store, &report, now).await?;
        Some(report)
    } else {
        None
    };

    finish(store, &sink, config, trader.clock().now()).await?;
    Ok(TickOutcome {
        fills,
        mode_change: observed.mode_change,
        market_open,
        rebalance,
        kill_cancelled: kill.cancelled,
    })
}

/// Runs one `session-close` for the newest session that closed no more than
/// `MAX_CLOSE_AGE_NANOS` before now. See `docs/phases/phase-6.md`, "session-close".
///
/// Order: enforce the kill switch → verify the signature → lock → ingest → restore →
/// reconcile → observe → (re-read the switch) → rebalance on a month end or when one is owed
/// → finish. The kill switch is enforced first, as in [`tick`].
pub async fn session_close<M: Market, B: Broker, C: Clock>(
    config: &EngineConfig,
    market: &M,
    store: &Store,
    sink: StoreWriter,
    broker: B,
    clock: C,
) -> Result<SessionCloseOutcome, AppError> {
    enforce_kill(
        store,
        &broker,
        flatten_on_kill(&config.limits_path),
        clock.now(),
    )
    .await?;
    // Before the lock: a deployment that must be signed and is not trades nothing.
    verify_from_env(config)?;
    let now = clock.now();
    take_engine_lock(store).await?;

    let today = utc_date(now)?;
    let calendar = market
        .calendar(config.history_start, plus_margin(today)?)
        .await?;
    let Some(session) = newest_closed_session(&calendar, now).copied() else {
        tracing::info!(
            now = now.as_u64(),
            "no session closed within the last six hours; nothing to do"
        );
        return Ok(SessionCloseOutcome::NoSession);
    };
    let run_key = format!("session-close/{}", session.date);

    let symbols = config.fetch_symbols();
    let ingest = step(
        store,
        &run_key,
        INGEST_STEP,
        now,
        ingest_session(
            market,
            &calendar,
            &symbols,
            config.history_start,
            &session,
            &config.bars_dir,
            now,
        ),
        |outcome| {
            json!({
                "skipped": outcome.skipped,
                "bars": outcome.bars,
                "path": outcome.path.display().to_string(),
            })
        },
    )
    .await?;

    let Restored { mut trader, .. } = restore(config, store, sink.clone(), broker, clock).await?;

    let reconciled = step(
        store,
        &run_key,
        RECONCILE_STEP,
        now,
        reconcile_pass(&mut trader),
        |report| {
            json!({
                "fills": report.fills.len(),
                "state_changes": report.state_changes.len(),
            })
        },
    )
    .await?;

    let observed = step(
        store,
        &run_key,
        OBSERVE_STEP,
        now,
        observe_pass(&mut trader, store, now),
        |observed| {
            json!({
                "equity": observed.equity.to_string(),
                "stepped_down": observed.stepped_down,
            })
        },
    )
    .await?;

    let rebalance = rebalance_step(
        &mut trader,
        store,
        &calendar,
        &session,
        &run_key,
        &observed,
        now,
    )
    .await?;

    finish(store, &sink, config, trader.clock().now()).await?;
    Ok(SessionCloseOutcome::Ran {
        session_date: session.date,
        ingest,
        fills: reconciled.fills.len(),
        mode_change: observed.mode_change,
        rebalance,
    })
}

/// The rebalance step of a session-close run: skipped when already `done`, recorded `done`
/// with `"due": false` when nothing is owed, recorded `done` with `"halted": true` (and the
/// rebalance marked as owed) when no order could go out, otherwise run as a recorded step.
async fn rebalance_step<B: Broker, C: Clock>(
    trader: &mut EngineTrader<'_, B, C>,
    store: &Store,
    calendar: &MarketCalendar,
    session: &Session,
    run_key: &str,
    observed: &Observed,
    now: UnixNanos,
) -> Result<RebalanceStep, AppError> {
    if matches!(
        store.step_status(run_key, REBALANCE_STEP).await?,
        Some((StepStatus::Done, _))
    ) {
        return Ok(RebalanceStep::AlreadyDone);
    }

    let owed = resize_owed(store).await? || trader.needs_flatten();
    let month_end = match calendar.is_last_session_of_month(session.date) {
        Some(month_end) => month_end,
        // The calendar does not reach the session after this one. An owed rebalance does not
        // need the answer; otherwise guessing "not a month end" would skip the monthly
        // rebalance without a trace, so the step fails.
        None if owed => false,
        None => {
            let err = AppError::Market(format!(
                "the calendar does not say whether {} ends its month",
                session.date
            ));
            store
                .record_step(
                    run_key,
                    REBALANCE_STEP,
                    StepStatus::Failed,
                    &json!({"error": err.to_string()}),
                    now,
                )
                .await?;
            return Err(err);
        }
    };
    let due = month_end || owed;
    if !due {
        store
            .record_step(
                run_key,
                REBALANCE_STEP,
                StepStatus::Done,
                &json!({"due": false}),
                now,
            )
            .await?;
        return Ok(RebalanceStep::NotDue);
    }
    // Re-read the switch right before the rebalance: a row written after this run restored
    // must stop it.
    if refresh_halts(trader, store, observed).await {
        // Deciding now would only write decisions whose orders are all refused. The
        // rebalance stays owed and is made by the first open tick that can trade.
        owe_rebalance(store, json!({"reason": "halted at the close"}), now).await?;
        store
            .record_step(
                run_key,
                REBALANCE_STEP,
                StepStatus::Done,
                &json!({"due": true, "halted": true}),
                now,
            )
            .await?;
        return Ok(RebalanceStep::Halted);
    }

    let report = step(
        store,
        run_key,
        REBALANCE_STEP,
        now,
        rebalance_pass(trader),
        rebalance_detail,
    )
    .await?;
    settle_or_owe(store, &report, now).await?;
    Ok(RebalanceStep::Ran(report))
}

/// The detail recorded with a rebalance that ran.
fn rebalance_detail(report: &RebalanceReport) -> Value {
    json!({
        "due": true,
        "decisions": report.decisions.len(),
        "orders_submitted": report.orders_submitted,
        "orders_failed": report.orders_failed,
        "orders_blocked": report.orders_blocked,
    })
}

/// Whether no order could go out now: the kill switch is engaged and the limits do not let
/// exits through it, or the broker has blocked the account.
fn halted<B: Broker, C: Clock>(trader: &EngineTrader<'_, B, C>, observed: &Observed) -> bool {
    let gate = trader.risk();
    (gate.state().kill_switch_engaged && !gate.limits().flatten_on_kill) || observed.trading_blocked
}

/// Re-reads the kill switch from the store into the trader's gate, then says whether trading
/// is halted. Called immediately before a rebalance: the switch may have been engaged by a
/// row written after this run restored. An unreadable switch counts as engaged.
async fn refresh_halts<B: Broker, C: Clock>(
    trader: &mut EngineTrader<'_, B, C>,
    store: &Store,
    observed: &Observed,
) -> bool {
    trader.refresh_kill_switch(store).await;
    halted(trader, observed)
}

/// The UTC hour of `now`.
fn utc_hour(now: UnixNanos) -> u64 {
    now.as_u64() / NANOS_PER_SECOND / 3_600 % 24
}

/// The newest session whose close is at or before `now`, if it closed no more than
/// `MAX_CLOSE_AGE_NANOS` ago. Closes are increasing in the calendar, so this is a binary search.
fn newest_closed_session(calendar: &MarketCalendar, now: UnixNanos) -> Option<&Session> {
    let sessions = calendar.sessions();
    let closed = sessions.partition_point(|session| session.close <= now);
    let session = sessions.get(closed.checked_sub(1)?)?;
    let age = now.as_u64().saturating_sub(session.close.as_u64());
    (age <= MAX_CLOSE_AGE_NANOS).then_some(session)
}

/// The UTC calendar date of `now`. Out of range is a `Market` error.
fn utc_date(now: UnixNanos) -> Result<NaiveDate, AppError> {
    let out_of_range = || AppError::Market(format!("time {} ns is out of range", now.as_u64()));
    let secs = i64::try_from(now.as_u64() / NANOS_PER_SECOND).map_err(|_| out_of_range())?;
    let nanos = u32::try_from(now.as_u64() % NANOS_PER_SECOND).map_err(|_| out_of_range())?;
    DateTime::from_timestamp(secs, nanos)
        .map(|time| time.date_naive())
        .ok_or_else(out_of_range)
}

/// `date` moved back by the calendar margin.
fn minus_margin(date: NaiveDate) -> Result<NaiveDate, AppError> {
    date.checked_sub_days(Days::new(CALENDAR_MARGIN_DAYS))
        .ok_or_else(|| AppError::Market(format!("{date} is out of range")))
}

/// `date` moved forward by the calendar margin.
fn plus_margin(date: NaiveDate) -> Result<NaiveDate, AppError> {
    date.checked_add_days(Days::new(CALENDAR_MARGIN_DAYS))
        .ok_or_else(|| AppError::Market(format!("{date} is out of range")))
}

/// Reconciles with the broker. The fills found count toward the day-trade limit inside the
/// trader.
async fn reconcile_pass<B: Broker, C: Clock>(
    trader: &mut EngineTrader<'_, B, C>,
) -> Result<SyncReport, AppError> {
    Ok(trader.reconcile().await?)
}

/// The equity observed, the mode change it caused and whether that was a step down.
struct Observed {
    equity: Decimal,
    /// The broker will not accept orders for the account.
    trading_blocked: bool,
    mode_change: Option<(DrawdownMode, DrawdownMode)>,
    stepped_down: bool,
}

/// Reads the account equity and feeds the drawdown state with it. A step down (to a smaller
/// exposure scale) is recorded as an owed resize at once, so that it survives a restart and a
/// closed market. A step up waits for the month end, as in the backtest.
async fn observe_pass<B: Broker, C: Clock>(
    trader: &mut EngineTrader<'_, B, C>,
    store: &Store,
    now: UnixNanos,
) -> Result<Observed, AppError> {
    let account = trader
        .broker()
        .account()
        .await
        .map_err(|e| EngineError::Broker(Box::new(e)))?;
    let equity = account.equity.as_decimal();
    let mode_change = trader.observe_equity(equity).await?;
    let step_down = mode_change.filter(|(from, to)| to.scale() < from.scale());
    if let Some((from, to)) = step_down {
        owe_rebalance(
            store,
            json!({"from": from.as_str(), "to": to.as_str()}),
            now,
        )
        .await?;
    }
    Ok(Observed {
        equity,
        trading_blocked: account.trading_blocked,
        mode_change,
        stepped_down: step_down.is_some(),
    })
}

/// Decides for every symbol and sends the orders.
async fn rebalance_pass<B: Broker, C: Clock>(
    trader: &mut EngineTrader<'_, B, C>,
) -> Result<RebalanceReport, AppError> {
    Ok(trader.rebalance().await?)
}

/// Whether a resize is owed: the newest status of the resize step is `started`.
async fn resize_owed(store: &Store) -> Result<bool, AppError> {
    Ok(matches!(
        store.step_status(RESIZE_RUN_KEY, RESIZE_STEP).await?,
        Some((StepStatus::Started, _))
    ))
}

/// Records that a rebalance is owed (`started`), with why. A second reason while one is
/// already owed is recorded too: the newest row is still `started`.
async fn owe_rebalance(store: &Store, detail: Value, now: UnixNanos) -> Result<(), AppError> {
    store
        .record_step(
            RESIZE_RUN_KEY,
            RESIZE_STEP,
            StepStatus::Started,
            &detail,
            now,
        )
        .await?;
    Ok(())
}

/// After a rebalance: if every order it wanted went out, an owed rebalance is settled
/// (`done`). If any order was blocked by the gate or refused by the broker, the book is not
/// at its target, so the rebalance is owed (again).
async fn settle_or_owe(
    store: &Store,
    report: &RebalanceReport,
    now: UnixNanos,
) -> Result<(), AppError> {
    let owed = resize_owed(store).await?;
    if report.orders_blocked == 0 && report.orders_failed == 0 {
        if owed {
            store
                .record_step(
                    RESIZE_RUN_KEY,
                    RESIZE_STEP,
                    StepStatus::Done,
                    &json!({}),
                    now,
                )
                .await?;
        }
    } else if !owed {
        owe_rebalance(
            store,
            json!({
                "reason": "orders blocked or refused",
                "orders_blocked": report.orders_blocked,
                "orders_failed": report.orders_failed,
            }),
            now,
        )
        .await?;
    }
    Ok(())
}

/// Flushes the record queue, then writes this process's heartbeat, stamped with the time of
/// the write (`now`), not the time the run began: the dead-man check measures from it.
async fn finish(
    store: &Store,
    sink: &StoreWriter,
    config: &EngineConfig,
    now: UnixNanos,
) -> Result<(), AppError> {
    sink.flush().await?;
    store.heartbeat(&config.source, now).await?;
    Ok(())
}

/// Records `started`, awaits `work`, then records `done` with `detail(&value)`, or `failed`
/// with the error text. Returns the original error on failure.
async fn step<T>(
    store: &Store,
    run_key: &str,
    name: &str,
    now: UnixNanos,
    work: impl Future<Output = Result<T, AppError>>,
    detail: impl FnOnce(&T) -> Value,
) -> Result<T, AppError> {
    store
        .record_step(run_key, name, StepStatus::Started, &json!({}), now)
        .await?;
    match work.await {
        Ok(value) => {
            store
                .record_step(run_key, name, StepStatus::Done, &detail(&value), now)
                .await?;
            Ok(value)
        }
        Err(err) => {
            let body = json!({"error": err.to_string()});
            if let Err(record_err) = store
                .record_step(run_key, name, StepStatus::Failed, &body, now)
                .await
            {
                tracing::error!(
                    run_key,
                    step = name,
                    error = %record_err,
                    "could not record the failed step"
                );
            }
            Err(err)
        }
    }
}
