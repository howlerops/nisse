//! Where the calendar and the daily bars come from: a trait, so the session logic can be tested without a broker.

use std::future::Future;

use chrono::NaiveDate;
use nisse_alpaca_adapter::{AlpacaConfig, AlpacaData, AlpacaHttp, fetch_calendar};
use nisse_core::MarketCalendar;
use nisse_core::nautilus::{Bar, Symbol, UnixNanos};

use crate::AppError;

/// Where the engine gets the trading calendar and daily bars. The Alpaca implementation is
/// [`AlpacaMarket`]; tests use a fake.
pub trait Market: Send + Sync {
    /// The trading calendar for `start..=end`.
    fn calendar(
        &self,
        start: NaiveDate,
        end: NaiveDate,
    ) -> impl Future<Output = Result<MarketCalendar, AppError>> + Send;

    /// Daily bars of `symbols` for `start..=end`, each stamped with its session's close in
    /// `calendar`; a bar whose session closes after `as_of` is left out. Sorted by symbol, then time.
    fn daily_bars(
        &self,
        symbols: &[Symbol],
        start: NaiveDate,
        end: NaiveDate,
        calendar: &MarketCalendar,
        as_of: UnixNanos,
    ) -> impl Future<Output = Result<Vec<Bar>, AppError>> + Send;
}

/// The Alpaca market data and calendar endpoints.
#[derive(Debug)]
pub struct AlpacaMarket {
    http: AlpacaHttp,
    config: AlpacaConfig,
    data: AlpacaData,
}

impl AlpacaMarket {
    /// Builds the market over `http`, reading the calendar from the trading API and the bars
    /// from the data API in `config`.
    pub fn new(http: AlpacaHttp, config: AlpacaConfig) -> Self {
        let data = AlpacaData::new(http.clone(), &config);
        Self { http, config, data }
    }
}

impl Market for AlpacaMarket {
    async fn calendar(&self, start: NaiveDate, end: NaiveDate) -> Result<MarketCalendar, AppError> {
        Ok(fetch_calendar(&self.http, &self.config, start, end).await?)
    }

    async fn daily_bars(
        &self,
        symbols: &[Symbol],
        start: NaiveDate,
        end: NaiveDate,
        calendar: &MarketCalendar,
        as_of: UnixNanos,
    ) -> Result<Vec<Bar>, AppError> {
        let names: Vec<&str> = symbols.iter().map(Symbol::as_str).collect();
        Ok(self
            .data
            .daily_bars(&names, start, end, calendar, as_of)
            .await?)
    }
}
