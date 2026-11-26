//! Alpaca market data REST client: historical bars and latest quotes.

use chrono::NaiveDate;
use nisse_core::nautilus::{Bar, QuoteTick};

use crate::{AdapterError, AlpacaConfig, AlpacaHttp, Feed};

/// Venue code used in every instrument id this adapter produces (`SPY.ALPACA`).
pub const VENUE: &str = "ALPACA";

/// Market data over Alpaca's data API, on the configured feed.
#[derive(Debug, Clone)]
pub struct AlpacaData {
    http: AlpacaHttp,
    base_url: String,
    feed: Feed,
}

impl AlpacaData {
    #[must_use]
    pub fn new(http: AlpacaHttp, config: &AlpacaConfig) -> Self {
        Self {
            http,
            base_url: config.data_base_url.clone(),
            feed: config.feed,
        }
    }
}

#[allow(unused_variables, reason = "stub: Phase 1 task T2")]
impl AlpacaData {
    /// Daily bars for `symbols` from `start` to `end` (inclusive dates), fully adjusted
    /// (`adjustment=all`), following `next_page_token` until exhausted. The result is sorted
    /// by symbol, then time. Each bar is `{SYMBOL}.ALPACA-1-DAY-LAST-EXTERNAL`; `ts_event` is
    /// the bar's `t` and `ts_init` equals it (no wall-clock read).
    pub async fn daily_bars(
        &self,
        symbols: &[&str],
        start: NaiveDate,
        end: NaiveDate,
    ) -> Result<Vec<Bar>, AdapterError> {
        todo!("Phase 1 task T2")
    }

    /// The latest quote for each symbol, sorted by symbol. A symbol with no quote, or with a
    /// zero bid or ask (Alpaca's "no active quote on that side"), is left out.
    pub async fn latest_quotes(&self, symbols: &[&str]) -> Result<Vec<QuoteTick>, AdapterError> {
        todo!("Phase 1 task T2")
    }
}
