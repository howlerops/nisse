//! Acceptance tests for the backtest loop (phase 3, BT1): synthetic bars through the trader.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "integration test")]

use std::collections::{BTreeMap, BTreeSet};

use nisse_core::{
    Action, Decision, Fill, MarketCalendar, StrategyVersionId,
    nautilus::{
        AggregationSource, Bar, BarAggregation, BarSpecification, BarType, InstrumentId, Price,
        PriceType, Quantity, Symbol, UnixNanos, Venue,
    },
};
use nisse_engine::{
    BacktestConfig, BacktestRun, CostModel, EngineError, ReplayOutcome, replay, run_backtest,
    strategies::{TrendParams, TrendVolTarget},
};
use rust_decimal::{Decimal, RoundingStrategy};

const NANOS_PER_SECOND: u64 = 1_000_000_000;
const SECONDS_PER_DAY: u64 = 86_400;
/// 2023-01-02, a Monday, as days since 1970-01-01.
const JAN_2_2023: i64 = 19_359;
/// 2023-01-02 to 2023-06-30 inclusive: 130 weekdays.
const SESSIONS: usize = 130;
/// The first month end in the data is 2023-01-31, the 22nd session (index 21).
const FIRST_MONTH_END: usize = 21;

// ---- calendar and synthetic data ------------------------------------------------------------

/// Days since 1970-01-01 to `(year, month, day)` in the proleptic Gregorian calendar.
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (
        year,
        u32::try_from(month).unwrap(),
        u32::try_from(day).unwrap(),
    )
}

/// 1970-01-01 was a Thursday. Weekdays are 1 (Monday) to 5 (Friday) on this count.
fn is_weekday(day: i64) -> bool {
    (1..=5).contains(&(day + 4).rem_euclid(7))
}

/// The close of the given UTC day, at 21:00 UTC (16:00 New York time in winter).
fn close_on(day: u64) -> UnixNanos {
    UnixNanos::from((day * SECONDS_PER_DAY + 21 * 3_600) * NANOS_PER_SECOND)
}

/// `n` consecutive weekday closes, the first on or after `start_day`.
fn weekdays(start_day: i64, n: usize) -> Vec<UnixNanos> {
    let mut out = Vec::with_capacity(n);
    let mut day = start_day;
    while out.len() < n {
        if is_weekday(day) {
            out.push(close_on(u64::try_from(day).unwrap()));
        }
        day += 1;
    }
    out
}

fn day_of(ts: UnixNanos) -> i64 {
    i64::try_from(ts.as_u64() / (SECONDS_PER_DAY * NANOS_PER_SECOND)).unwrap()
}

fn month_of(ts: UnixNanos) -> (i64, u32) {
    let (year, month, _) = civil(day_of(ts));
    (year, month)
}

fn closes() -> Vec<UnixNanos> {
    weekdays(JAN_2_2023, SESSIONS)
}

fn calendar_for(closes: &[UnixNanos]) -> MarketCalendar {
    MarketCalendar::from_closes(closes).unwrap()
}

fn dec(s: &str) -> Decimal {
    s.parse().unwrap()
}

fn sym(s: &str) -> Symbol {
    Symbol::new(s)
}

fn symbol_of(bar: &Bar) -> Symbol {
    bar.bar_type.instrument_id().symbol
}

/// A price at exactly 4 decimal places (a bar's prices must share one precision).
fn px(d: Decimal) -> Price {
    let mut at_4dp = d.round_dp(4);
    at_4dp.rescale(4);
    Price::from(at_4dp.to_string().as_str())
}

/// One bar per close. Open is the previous close times 1.002 (the first open is `start_px`),
/// close is the previous close times `1 + daily_change` plus or minus 0.1% on alternate days.
fn series(
    symbol: &str,
    closes: &[UnixNanos],
    start_px: Decimal,
    daily_change: Decimal,
) -> Vec<Bar> {
    let wobble = dec("0.001");
    let open_gap = dec("1.002");
    let instrument_id = InstrumentId::new(Symbol::new(symbol), Venue::new("ALPACA"));
    let spec = BarSpecification::new(1, BarAggregation::Day, PriceType::Last);
    let bar_type = BarType::new(instrument_id, spec, AggregationSource::External);
    let mut previous = start_px;
    closes
        .iter()
        .enumerate()
        .map(|(i, &ts)| {
            let open = if i == 0 {
                start_px
            } else {
                (previous * open_gap).round_dp(4)
            };
            let sign = if i % 2 == 0 {
                Decimal::ONE
            } else {
                -Decimal::ONE
            };
            let close = (previous * (Decimal::ONE + daily_change + sign * wobble)).round_dp(4);
            previous = close;
            Bar::new(
                bar_type,
                px(open),
                px(open.max(close)),
                px(open.min(close)),
                px(close),
                Quantity::from("1000"),
                ts,
                ts,
            )
        })
        .collect()
}

/// All the bars, sorted by time then symbol.
fn merge(series: Vec<Vec<Bar>>) -> Vec<Bar> {
    let mut bars: Vec<Bar> = series.into_iter().flatten().collect();
    bars.sort_by(|a, b| {
        let (sa, sb) = (symbol_of(a), symbol_of(b));
        (a.ts_event.as_u64(), sa.as_str()).cmp(&(b.ts_event.as_u64(), sb.as_str()))
    });
    bars
}

/// FALL (down about 0.5% a day) and RISE (up about 0.5% a day), every session.
fn market(closes: &[UnixNanos]) -> Vec<Bar> {
    merge(vec![
        series("FALL", closes, dec("100"), dec("-0.005")),
        series("RISE", closes, dec("100"), dec("0.005")),
    ])
}

/// The market plus LATE, which has bars only from the 31st session on.
fn with_late(closes: &[UnixNanos]) -> Vec<Bar> {
    merge(vec![
        series("FALL", closes, dec("100"), dec("-0.005")),
        series("LATE", &closes[30..], dec("50"), dec("0.005")),
        series("RISE", closes, dec("100"), dec("0.005")),
    ])
}

// ---- strategy and run -----------------------------------------------------------------------

fn params() -> TrendParams {
    TrendParams {
        lookback_days: 20,
        skip_days: 2,
        vol_window: 10,
        target_vol: dec("0.09"),
        max_weight: dec("0.5"),
        min_trade_notional: Decimal::from(25),
        sessions_per_year: 252,
    }
}

fn strategy() -> TrendVolTarget {
    TrendVolTarget::new(params()).unwrap()
}

fn config(costs: CostModel) -> BacktestConfig {
    BacktestConfig {
        strategy_version_id: StrategyVersionId(1),
        window: strategy().window_spec(),
        starting_cash: Decimal::from(100_000),
        costs,
        data_snapshot_id: "snap-bt1".to_owned(),
        seed: 7,
    }
}

fn free() -> CostModel {
    CostModel::new(Decimal::ZERO, Decimal::ZERO).unwrap()
}

async fn backtest(bars: &[Bar], calendar: &MarketCalendar, costs: CostModel) -> BacktestRun {
    run_backtest(strategy(), bars, calendar, config(costs))
        .await
        .unwrap()
}

/// The 1-based decision id carried by a fill's client order id `nisse-<id>-0`.
fn decision_id(fill: &Fill) -> usize {
    fill.client_order_id
        .as_str()
        .strip_prefix("nisse-")
        .and_then(|rest| rest.strip_suffix("-0"))
        .and_then(|n| n.parse::<usize>().ok())
        .unwrap()
}

fn decision_for<'a>(run: &'a BacktestRun, fill: &Fill) -> &'a Decision {
    run.decisions.get(decision_id(fill) - 1).unwrap()
}

/// Buys are positive, sells negative.
fn signed_qty(run: &BacktestRun, fill: &Fill) -> Decimal {
    let qty = fill.qty.as_decimal();
    if decision_for(run, fill).intended_qty < Decimal::ZERO {
        -qty
    } else {
        qty
    }
}

fn bar_at(bars: &[Bar], ts: UnixNanos, symbol: Symbol) -> &Bar {
    bars.iter()
        .find(|b| b.ts_event == ts && symbol_of(b) == symbol)
        .unwrap()
}

fn ceil_to_cent(x: Decimal) -> Decimal {
    x.round_dp_with_strategy(2, RoundingStrategy::ToPositiveInfinity)
}

// ---- calendar helper checks -----------------------------------------------------------------

#[test]
fn civil_days_match_known_dates() {
    assert_eq!(civil(0), (1970, 1, 1));
    assert_eq!(civil(19_358), (2023, 1, 1));
    assert_eq!(civil(JAN_2_2023), (2023, 1, 2));
    assert_eq!(civil(19_782), (2024, 2, 29));
    assert_eq!(civil(day_of(*closes().last().unwrap())), (2023, 6, 30));
}

#[test]
fn weekday_helper_skips_weekends() {
    assert!(is_weekday(0), "1970-01-01 is a Thursday");
    assert!(!is_weekday(19_358), "2023-01-01 is a Sunday");
    assert!(is_weekday(JAN_2_2023), "2023-01-02 is a Monday");
    let closes = closes();
    assert_eq!(closes.len(), SESSIONS);
    assert!(closes.iter().all(|c| is_weekday(day_of(*c))));
}

// ---- BT1 ------------------------------------------------------------------------------------

#[tokio::test]
async fn rising_asset_is_entered_at_the_first_month_end_and_held() {
    let closes = closes();
    let run = backtest(&market(&closes), &calendar_for(&closes), free()).await;
    let rise = sym("RISE");

    let enter = run
        .decisions
        .iter()
        .find(|d| d.symbol == rise && d.action == Action::Enter)
        .unwrap();
    assert_eq!(enter.ts, closes[FIRST_MONTH_END]);

    let held: Decimal = run
        .fills
        .iter()
        .filter(|f| decision_for(&run, f).symbol == rise)
        .map(|f| signed_qty(&run, f))
        .sum();
    assert!(held > Decimal::ZERO, "held {held}");
}

#[tokio::test]
async fn falling_asset_is_never_bought() {
    let closes = closes();
    let run = backtest(&market(&closes), &calendar_for(&closes), free()).await;
    let fall = sym("FALL");

    assert!(
        run.fills
            .iter()
            .all(|f| decision_for(&run, f).symbol != fall)
    );
    let own: Vec<&Decision> = run.decisions.iter().filter(|d| d.symbol == fall).collect();
    assert!(!own.is_empty());
    for d in own {
        let allowed = (d.action == Action::Hold && d.reason_code == "within_band")
            || d.action == Action::Skip;
        assert!(allowed, "{d:?}");
    }
}

#[tokio::test]
async fn fills_happen_at_the_next_open_never_at_the_deciding_close() {
    let closes = closes();
    let bars = market(&closes);
    let run = backtest(&bars, &calendar_for(&closes), free()).await;
    assert!(!run.fills.is_empty());

    for fill in &run.fills {
        let decision = decision_for(&run, fill);
        let decided_at = decision.ts;
        // The fill is at the next session's close time, which is the next session's open.
        let next_close = closes
            .iter()
            .copied()
            .find(|c| c.as_u64() > decided_at.as_u64())
            .unwrap();
        assert_eq!(fill.ts, next_close);

        let fill_bar = bar_at(&bars, fill.ts, decision.symbol);
        let decided_bar = bar_at(&bars, decided_at, decision.symbol);
        assert_eq!(fill.px.as_decimal(), fill_bar.open.as_decimal());
        assert_ne!(fill.px.as_decimal(), decided_bar.close.as_decimal());
    }
}

#[tokio::test]
async fn equity_is_cash_while_flat() {
    let closes = closes();
    let run = backtest(&market(&closes), &calendar_for(&closes), free()).await;
    let first_fill = run.fills.first().unwrap().ts.as_u64();

    let flat: Vec<_> = run
        .equity_curve
        .iter()
        .filter(|(ts, _)| ts.as_u64() < first_fill)
        .collect();
    assert!(
        flat.len() >= FIRST_MONTH_END,
        "only {} flat sessions",
        flat.len()
    );
    for (_, equity) in flat {
        assert_eq!(*equity, Decimal::from(100_000));
    }
}

#[tokio::test]
async fn fees_paid_is_the_sum_of_ceil_to_cent_per_fill() {
    let closes = closes();
    let costs = CostModel::new(Decimal::ZERO, dec("0.005")).unwrap();
    let run = backtest(&market(&closes), &calendar_for(&closes), costs).await;

    let expected: Decimal = run
        .fills
        .iter()
        .map(|f| ceil_to_cent(f.qty.as_decimal() * dec("0.005")))
        .sum();
    assert!(expected > Decimal::ZERO);
    assert_eq!(run.fees_paid, expected);
}

#[tokio::test]
async fn one_equity_point_per_session_in_time_order() {
    let closes = closes();
    let run = backtest(&market(&closes), &calendar_for(&closes), free()).await;

    assert_eq!(run.equity_curve.len(), closes.len());
    for (point, close) in run.equity_curve.iter().zip(&closes) {
        assert_eq!(point.0, *close);
    }
    assert!(
        run.equity_curve
            .windows(2)
            .all(|w| w[0].0.as_u64() < w[1].0.as_u64())
    );
}

#[tokio::test]
async fn two_runs_on_the_same_inputs_are_equal() {
    let closes = closes();
    let bars = market(&closes);
    let calendar = calendar_for(&closes);
    let costs = CostModel::new(dec("5"), dec("0.005")).unwrap();
    let first = backtest(&bars, &calendar, costs).await;
    let second = backtest(&bars, &calendar, costs).await;
    assert!(!first.fills.is_empty());
    assert_eq!(first, second);
}

#[tokio::test]
async fn every_decision_replays_as_a_match() {
    let closes = closes();
    let run = backtest(&market(&closes), &calendar_for(&closes), free()).await;
    assert!(!run.decisions.is_empty());
    for decision in &run.decisions {
        assert_eq!(replay(&strategy(), decision), ReplayOutcome::Match);
    }
}

#[tokio::test]
async fn equity_change_is_position_times_price_change_without_costs() {
    let closes = closes();
    let bars = market(&closes);
    let run = backtest(&bars, &calendar_for(&closes), free()).await;
    let close_at: BTreeMap<(u64, Symbol), Decimal> = bars
        .iter()
        .map(|b| ((b.ts_event.as_u64(), symbol_of(b)), b.close.as_decimal()))
        .collect();

    let mut position: BTreeMap<Symbol, Decimal> = BTreeMap::new();
    let mut checked: Vec<usize> = Vec::new();
    for t in 0..closes.len() {
        let ts = closes[t].as_u64();
        if t > 0 {
            // No fill at this session: the position held over (t-1, t] is the one after the
            // fills at or before t-1, and the equity change is its mark-to-market.
            let filled_now = run.fills.iter().any(|f| f.ts.as_u64() == ts);
            if !filled_now {
                let prev_ts = closes[t - 1].as_u64();
                let delta = run.equity_curve[t].1 - run.equity_curve[t - 1].1;
                let expected: Decimal = position
                    .iter()
                    .map(|(s, q)| {
                        let now = close_at.get(&(ts, *s)).copied().unwrap();
                        let before = close_at.get(&(prev_ts, *s)).copied().unwrap();
                        *q * (now - before)
                    })
                    .sum();
                // The broker reports equity as `Money`, rounded to the cent, so each of the
                // two points can be off by half a cent.
                assert!(
                    (delta - expected).abs() <= dec("0.01"),
                    "session {t}: delta {delta} expected {expected}"
                );
                checked.push(t);
            }
        }
        for fill in run.fills.iter().filter(|f| f.ts.as_u64() == ts) {
            let s = decision_for(&run, fill).symbol;
            *position.entry(s).or_insert(Decimal::ZERO) += signed_qty(&run, fill);
        }
    }

    let last_fill = run.fills.iter().map(|f| f.ts.as_u64()).max().unwrap();
    for (t, close) in closes.iter().enumerate().skip(1) {
        if close.as_u64() > last_fill {
            assert!(
                checked.contains(&t),
                "session {t} after the last fill was not checked"
            );
        }
    }
    assert!(checked.contains(&(closes.len() - 1)));
}

#[tokio::test]
async fn a_symbol_that_starts_late_is_skipped_until_it_has_history() {
    let closes = closes();
    let run = backtest(&with_late(&closes), &calendar_for(&closes), free()).await;
    let late = sym("LATE");
    let own: Vec<&Decision> = run.decisions.iter().filter(|d| d.symbol == late).collect();

    // One decision per rebalance (five month ends before the final session).
    assert_eq!(own.len(), 5);
    // 2023-01-31: no bar yet.
    assert_eq!(own[0].action, Action::Skip);
    assert_eq!(own[0].reason_code, "missing_feature");
    // 2023-02-28: 12 bars, not enough for momentum.
    assert_eq!(own[1].action, Action::Skip);
    assert_eq!(own[1].reason_code, "insufficient_history");
    // 2023-03-31: 35 bars, rising, so it is entered.
    assert_eq!(own[2].action, Action::Enter);
    assert_eq!(own[2].reason_code, "trend_up");
}

#[tokio::test]
async fn the_final_session_never_rebalances() {
    let closes = closes();
    let run = backtest(&market(&closes), &calendar_for(&closes), free()).await;

    let rebalance_times: BTreeSet<u64> = run.decisions.iter().map(|d| d.ts.as_u64()).collect();
    let month_ends = (0..closes.len() - 1)
        .filter(|&i| month_of(closes[i]) != month_of(closes[i + 1]))
        .count();
    assert_eq!(month_ends, 5);
    assert_eq!(rebalance_times.len(), month_ends);
    assert!(!rebalance_times.contains(&closes.last().unwrap().as_u64()));
}

#[tokio::test]
async fn a_single_session_gives_one_equity_point_and_no_orders() {
    let closes = closes();
    let one = &closes[..1];
    let run = backtest(&market(one), &calendar_for(one), free()).await;
    assert_eq!(run.equity_curve, vec![(closes[0], Decimal::from(100_000))]);
    assert!(run.decisions.is_empty());
    assert!(run.fills.is_empty());
    assert_eq!(run.fees_paid, Decimal::ZERO);
}

// ---- errors ---------------------------------------------------------------------------------

#[tokio::test]
async fn empty_bars_are_an_invalid_config() {
    let closes = closes();
    let result = run_backtest(strategy(), &[], &calendar_for(&closes), config(free())).await;
    assert!(matches!(result, Err(EngineError::InvalidConfig(_))));
}

#[tokio::test]
async fn unsorted_bars_are_an_invalid_bar() {
    let closes = closes();
    let mut bars = market(&closes);
    bars.reverse();
    let result = run_backtest(strategy(), &bars, &calendar_for(&closes), config(free())).await;
    assert!(matches!(result, Err(EngineError::InvalidBar(_))));
}

#[tokio::test]
async fn a_duplicate_time_and_symbol_is_an_invalid_bar() {
    let closes = closes();
    let mut bars = market(&closes);
    let copy = bars[5];
    bars.insert(6, copy);
    let result = run_backtest(strategy(), &bars, &calendar_for(&closes), config(free())).await;
    assert!(matches!(result, Err(EngineError::InvalidBar(_))));
}

#[tokio::test]
async fn a_bar_outside_the_calendar_is_an_invalid_bar() {
    let closes = closes();
    let bars = market(&closes);

    // The calendar stops one session early, so the last bar has no session.
    let short = calendar_for(&closes[..SESSIONS - 1]);
    let result = run_backtest(strategy(), &bars, &short, config(free())).await;
    assert!(matches!(
        result,
        Err(EngineError::InvalidBar("bar outside the calendar"))
    ));

    // A session missing from the middle of the calendar.
    let mut gap = closes.clone();
    gap.remove(50);
    let result = run_backtest(strategy(), &bars, &calendar_for(&gap), config(free())).await;
    assert!(matches!(
        result,
        Err(EngineError::InvalidBar("bar outside the calendar"))
    ));
}
