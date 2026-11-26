//! `nisse-replay`: replays the decisions made by this build and prints the report.
//!
//! Reads `DATABASE_URL`. Exits 0 when the replay is clean and 1 otherwise. Run it through
//! `scripts/replay.sh`.

use nisse_core::{Clock, DecisionId, SystemClock};
use nisse_engine::{EngineError, GIT_SHA, Strategy};
use nisse_eval::{EvalError, replay::replay_decisions};
use serde_json::Value;

/// Builds a strategy from its stored parameters with the engine's own registry.
fn registry(params: &Value) -> Result<Box<dyn Strategy>, EvalError> {
    match nisse_engine::strategies::build(params) {
        Ok(built) => Ok(built.strategy),
        Err(EngineError::UnknownStrategy(name)) => Err(EvalError::UnknownStrategy(name)),
        Err(other) => Err(EvalError::UnknownStrategy(other.to_string())),
    }
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
