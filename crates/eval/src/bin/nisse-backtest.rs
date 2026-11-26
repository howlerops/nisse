//! `nisse-backtest`: runs the backtest described by a JSON config file and stores the result.
//!
//! Usage: `nisse-backtest <config.json> [--allow-dirty]`. Reads `DATABASE_URL`, which is never
//! printed (it may hold a password). Exits 2 on bad arguments. The logic is in
//! `nisse_eval::backtest_job`.

use anyhow::Context;
use nisse_core::{Clock, SystemClock};
use nisse_eval::backtest_job::{BacktestJob, run_backtest_job};
use nisse_store::Store;

const USAGE: &str = "usage: nisse-backtest <config.json> [--allow-dirty]";

/// The config path and the `--allow-dirty` flag, or `None` for any other arguments.
fn parse_args(args: &[String]) -> Option<(&str, bool)> {
    let mut path = None;
    let mut allow_dirty = false;
    for arg in args {
        if arg == "--allow-dirty" {
            allow_dirty = true;
        } else if path.is_none() && !arg.starts_with('-') {
            path = Some(arg.as_str());
        } else {
            return None;
        }
    }
    path.map(|path| (path, allow_dirty))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some((path, allow_dirty)) = parse_args(&args) else {
        eprintln!("{USAGE}");
        std::process::exit(2);
    };

    let text = std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?;
    let value: serde_json::Value =
        serde_json::from_str(&text).with_context(|| format!("{path} is not valid JSON"))?;
    let job = BacktestJob::from_json(&value)?;

    let url =
        std::env::var("DATABASE_URL").map_err(|_| anyhow::anyhow!("DATABASE_URL is not set"))?;
    // Deliberately not printed: the URL may contain a password.
    let store = Store::new(nisse_store::connect(&url).await?);

    let outcome = run_backtest_job(
        &store,
        &job,
        nisse_engine::build_is_replayable(),
        allow_dirty,
        SystemClock.now(),
    )
    .await?;

    println!("trial: {}", outcome.trial_id);
    println!("strategy_version: {}", outcome.strategy_version_id.0);
    println!("{}", serde_json::to_string_pretty(&outcome.result)?);
    Ok(())
}
