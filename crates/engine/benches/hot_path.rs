#![allow(clippy::unwrap_used, clippy::expect_used, reason = "benchmark setup")]

use criterion::{Criterion, criterion_group, criterion_main};
use nisse_engine::features::{RollingStats, WindowSpec};
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

criterion_group!(benches, rolling_push);
criterion_main!(benches);
