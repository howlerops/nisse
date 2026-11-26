//! `nisse-replay`: replays the decisions made by this build and prints the report.
//!
//! Reads `DATABASE_URL`. Exits 0 when the replay is clean and 1 otherwise. Run it through
//! `scripts/replay.sh`.

use nisse_core::{Clock, DecisionId, SystemClock};
use nisse_engine::{GIT_SHA, Strategy};
use nisse_eval::{EvalError, replay::replay_decisions};
use serde_json::Value;

/// Builds a strategy from its stored parameters. No strategies are registered yet: Phase 3
/// adds the real ones here, matched on the `"strategy"` field.
fn registry(params: &Value) -> Result<Box<dyn Strategy>, EvalError> {
    let name = params
        .get("strategy")
        .and_then(Value::as_str)
        .unwrap_or("<missing>");
    Err(EvalError::UnknownStrategy(name.to_owned()))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let url =
        std::env::var("DATABASE_URL").map_err(|_| anyhow::anyhow!("DATABASE_URL is not set"))?;
    // Deliberately not printed: the URL may contain a password.
    let client = nisse_store::connect(&url).await?;

    let now = SystemClock.now();
    let report = replay_decisions(&client, GIT_SHA, DecisionId(0), &registry, now).await?;

    println!("git sha: {GIT_SHA}");
    println!("matched: {}", report.matched);
    println!("failures: {}", report.failures.len());
    println!("other_builds: {}", report.other_builds);
    println!("unbuildable: {}", report.unbuildable);
    for failure in &report.failures {
        println!("decision {}: {:?}", failure.decision_id.0, failure.outcome);
    }

    if !report.is_clean() {
        std::process::exit(1);
    }
    Ok(())
}
