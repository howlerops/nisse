//! `nisse-score`: the nightly scoring job. Run it after the close, from cron.
//!
//! Usage: `nisse-score <gates.json> <bars_dir>`. Reads `DATABASE_URL` and
//! `NISSE_HOLDOUT_SALT` (at least 16 bytes; neither is ever printed). In order: assigns new
//! decisions to the decision holdout, writes outcome windows and decision-quality scores,
//! then execution and exit scores. Every step is idempotent, so a failed run is simply run
//! again. Exits 2 on bad arguments and non-zero on any error.

use std::path::Path;

use nisse_bars::BarReader;
use nisse_core::{Clock, SystemClock};
use nisse_eval::{
    execution_job::{score_execution, score_exits},
    gates::Gates,
    score_job::{assign_holdouts, score_outcomes},
};
use nisse_store::tables::scoring;

const USAGE: &str = "usage: nisse-score <gates.json> <bars_dir>";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    nisse_core::logging::init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [gates_path, bars_dir] = args.as_slice() else {
        eprintln!("{USAGE}");
        std::process::exit(2);
    };

    let gates = Gates::load(Path::new(gates_path))?;
    let bars = BarReader::open(Path::new(bars_dir))?;
    // Deliberately not printed: the salt decides which decisions agents never see, and the
    // URL may contain a password.
    let salt = std::env::var("NISSE_HOLDOUT_SALT")
        .map_err(|_| anyhow::anyhow!("NISSE_HOLDOUT_SALT is not set"))?;
    let url =
        std::env::var("DATABASE_URL").map_err(|_| anyhow::anyhow!("DATABASE_URL is not set"))?;
    let client = nisse_store::connect(&url).await?;
    // One run at a time: two overlapping runs would both read the same pending decisions and
    // write their scores twice. The lock is released when this process ends.
    if !scoring::try_advisory_lock(&client, scoring::SCORE_JOB_LOCK).await? {
        anyhow::bail!("another nisse-score run holds the job lock; nothing was done");
    }
    let now = SystemClock.now();

    let holdout = assign_holdouts(&client, salt.as_bytes(), now).await?;
    println!(
        "holdout: {} assigned, {} hidden",
        holdout.assigned, holdout.hidden
    );
    let outcomes = score_outcomes(&client, &bars, &gates, now).await?;
    println!(
        "outcomes: {} written, {} quality scores, {} waiting, {} unscored (base bar too old)",
        outcomes.written, outcomes.quality_scores, outcomes.waiting, outcomes.base_too_old
    );
    let execution = score_execution(&client, now).await?;
    println!(
        "execution: {} scored, {} waiting, {} without an arrival price",
        execution.decisions_scored, execution.waiting, execution.no_arrival
    );
    let exits = score_exits(&client, &bars, now).await?;
    println!(
        "exits: {} scored, {} open, {} shares sold without a matching buy",
        exits.entries_scored, exits.open, exits.unmatched_sell_qty
    );
    Ok(())
}
