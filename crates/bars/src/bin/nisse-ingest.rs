//! `nisse-ingest`: fetches daily bars from Alpaca and writes them as one Parquet file.
//!
//! ```text
//! nisse-ingest --symbols SPY,EFA --start 2015-01-01 --end 2024-12-31 --feed iex \
//!     --out data/bars/2024-12-31.parquet
//! ```
//!
//! Needs `APCA_API_KEY_ID` and `APCA_API_SECRET_KEY` in the environment. It is run by the owner
//! on the engine host and never by an agent. The `sip` feed needs a paid Alpaca data
//! subscription. Exits 0 after writing, 1 on a fetch or write error, 2 on bad arguments.

use std::process::ExitCode;
use std::time::Duration;

use nisse_alpaca_adapter::{AlpacaConfig, AlpacaData, AlpacaHttp, Credentials};
use nisse_bars::ingest::{IngestArgs, ingest};
use nisse_core::{Clock, SystemClock};

const USAGE: &str = "usage: nisse-ingest --symbols SPY,EFA --start YYYY-MM-DD --end YYYY-MM-DD \
    --feed iex|sip --out PATH";
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);

#[tokio::main]
async fn main() -> ExitCode {
    let args = match IngestArgs::parse(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("nisse-ingest: {message}");
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match run(&args).await {
        Ok(count) => {
            println!("wrote {count} bars to {}", args.out.display());
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("nisse-ingest: {err:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run(args: &IngestArgs) -> anyhow::Result<usize> {
    let credentials = Credentials::from_env()?;
    let config = AlpacaConfig::paper(args.feed);
    let http = AlpacaHttp::new(&credentials, HTTP_TIMEOUT)?;
    let data = AlpacaData::new(http.clone(), &config);
    ingest(&http, &config, &data, args, SystemClock.now()).await
}
