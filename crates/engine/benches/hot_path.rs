#![allow(clippy::unwrap_used, clippy::expect_used, reason = "benchmark setup")]

use criterion::{Criterion, criterion_group, criterion_main};
use nisse_core::{
    Features, ManualClock, StrategyVersionId,
    nautilus::{
        AggregationSource, Bar, BarAggregation, BarSpecification, BarType, InstrumentId, Price,
        PriceType, Quantity, Symbol, UnixNanos, Venue,
    },
};
use nisse_engine::Strategy;
use nisse_engine::features::{RollingStats, WindowSpec};
use nisse_engine::strategies::{TrendParams, TrendVolTarget, feature};
use nisse_engine::{
    CostModel, MemoryStore, OrderBookState, OrderGateway, SimBroker, SyncOptions, Trader,
    TraderConfig,
};
use rust_decimal::Decimal;
use std::hint::black_box;

/// Number of pre-generated closes that the measured loop cycles through.
const CLOSES: usize = 1024;
/// Closes pushed before measurement, so the window is full.
const WARM_UP: usize = 300;

/// Deterministic close `i`: a price between 100.00 and 119.99 that moves every bar.
fn close_at(i: usize) -> Decimal {
    let cents = 10_000 + (i as i64 * 7919) % 2_000;
    Decimal::new(cents, 2)
}

fn rolling_push(c: &mut Criterion) {
    let spec = WindowSpec {
        lookback_days: 252,
        skip_days: 21,
        vol_window: 60,
    };
    let closes: Vec<Decimal> = (0..CLOSES).map(close_at).collect();
    let mut stats = RollingStats::new(spec).unwrap();
    for &close in closes.iter().take(WARM_UP) {
        stats.push(close).unwrap();
    }

    c.bench_function("rolling_push", |b| {
        let mut idx = 0_usize;
        b.iter(|| {
            let close = closes.get(idx).copied().unwrap_or(Decimal::ONE);
            idx = (idx + 1) % CLOSES;
            black_box(stats.push(black_box(close))).unwrap();
        });
    });
}

fn trend_decide(c: &mut Criterion) {
    let strategy = TrendVolTarget::new(TrendParams {
        lookback_days: 252,
        skip_days: 21,
        vol_window: 60,
        target_vol: Decimal::new(9, 2),
        max_weight: Decimal::new(25, 2),
        min_trade_notional: Decimal::new(25, 0),
        sessions_per_year: 252,
    })
    .unwrap();
    let mut features = Features::new();
    for (name, value) in [
        (feature::CLOSE, Decimal::new(10_000, 2)),
        (feature::HISTORY_DAYS, Decimal::new(300, 0)),
        (feature::POSITION_QTY, Decimal::new(50, 0)),
        (feature::EQUITY, Decimal::new(100_000, 0)),
        (feature::N_ASSETS, Decimal::new(8, 0)),
        (feature::MOMENTUM, Decimal::new(10, 2)),
        (feature::VAR_DAILY, Decimal::new(101_587, 9)),
    ] {
        features.insert(name.to_owned(), value);
    }

    c.bench_function("trend_decide", |b| {
        b.iter(|| black_box(strategy.decide(black_box(&features), 0)));
    });
}

/// Symbols in the benchmarked universe.
const UNIVERSE: usize = 8;

/// One bar per symbol and session of the table, all with positive, varying closes.
fn trader_bar(symbol: Symbol, day: u64, close: Decimal) -> Bar {
    let instrument_id = InstrumentId::new(symbol, Venue::new("ALPACA"));
    let spec = BarSpecification::new(1, BarAggregation::Day, PriceType::Last);
    let bar_type = BarType::new(instrument_id, spec, AggregationSource::External);
    let price = Price::from_decimal(close).unwrap();
    let ts = UnixNanos::from(day * 86_400_000_000_000);
    Bar::new(
        bar_type,
        price,
        price,
        price,
        price,
        Quantity::from("1000"),
        ts,
        ts,
    )
}

fn trader_on_bar(c: &mut Criterion) {
    let strategy = TrendVolTarget::new(TrendParams {
        lookback_days: 252,
        skip_days: 21,
        vol_window: 60,
        target_vol: Decimal::new(9, 2),
        max_weight: Decimal::new(25, 2),
        min_trade_notional: Decimal::new(25, 0),
        sessions_per_year: 252,
    })
    .unwrap();
    let symbols: Vec<Symbol> = (0..UNIVERSE)
        .map(|i| Symbol::new(format!("S{i:02}")))
        .collect();
    let config = TraderConfig {
        strategy_version_id: StrategyVersionId(1),
        symbols: symbols.clone(),
        window: strategy.window_spec(),
        data_snapshot_id: "bench".to_owned(),
        seed: 0,
        sync: SyncOptions {
            never_arrived_grace_nanos: 0,
            never_arrived_recheck_nanos: 0,
        },
    };
    let store = MemoryStore::new();
    let mut trader = Trader::new(
        strategy,
        OrderGateway::new(
            SimBroker::new(
                Decimal::from(100_000),
                CostModel::new(Decimal::ZERO, Decimal::ZERO).unwrap(),
            ),
            &store,
        ),
        &store,
        &store,
        ManualClock::new(UnixNanos::from(1_000_u64)),
        config,
        OrderBookState::default(),
    )
    .unwrap();

    // Bars cycle over symbols and days: entry `i` is symbol `i % UNIVERSE`, on day `i / UNIVERSE`.
    let bars: Vec<Bar> = (0..CLOSES * UNIVERSE)
        .map(|i| {
            let symbol = &symbols[i % UNIVERSE];
            trader_bar(*symbol, (i / UNIVERSE) as u64, close_at(i))
        })
        .collect();
    for bar in bars.iter().take(WARM_UP * UNIVERSE) {
        trader.on_bar(bar).unwrap();
    }

    c.bench_function("trader_on_bar", |b| {
        let mut idx = WARM_UP * UNIVERSE;
        b.iter(|| {
            let bar = &bars[idx];
            idx = (idx + 1) % bars.len();
            black_box(trader.on_bar(black_box(bar))).unwrap();
        });
    });
}

criterion_group!(benches, rolling_push, trend_decide, trader_on_bar);
criterion_main!(benches);
