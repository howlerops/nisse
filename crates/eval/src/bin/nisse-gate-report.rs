//! `nisse-gate-report`: the promotion gate report for one strategy version.
//!
//! This report promotes nothing; a person reads it. It writes the verdicts as `eval_score`
//! rows (`gate-v1`) and prints the report as JSON.
//!
//! Usage: `nisse-gate-report <strategy_version_id> <gates.json>`. Reads `DATABASE_URL`, which
//! is never printed (it may hold a password). Exit codes: 0 when every gate passes, 1 when a
//! gate failed, 3 when none failed but some were not evaluated, 2 for bad arguments. Any other
//! error exits non-zero (1, as for any uncaught error from `main`). The logic is in
//! `nisse_eval::gate_report`.

use std::path::Path;

use nisse_core::{Clock, StrategyVersionId, SystemClock};
use nisse_eval::gate_report::{build_report, load_inputs, store_report};
use nisse_eval::gates::Gates;

const USAGE: &str = "usage: nisse-gate-report <strategy_version_id> <gates.json>";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (id, path) = match args.as_slice() {
        [id, path] => match id.parse::<i64>() {
            Ok(id) => (StrategyVersionId(id), path),
            Err(_) => {
                eprintln!("{USAGE}");
                std::process::exit(2);
            }
        },
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    };

    let gates = Gates::load(Path::new(path))?;
    let url =
        std::env::var("DATABASE_URL").map_err(|_| anyhow::anyhow!("DATABASE_URL is not set"))?;
    // Deliberately not printed: the URL may contain a password.
    let client = nisse_store::connect(&url).await?;

    let inputs = load_inputs(&client, id, gates).await?;
    let report = build_report(&inputs);
    store_report(&client, &report, SystemClock.now()).await?;
    println!("{}", serde_json::to_string_pretty(&report.to_json())?);

    let code = if report.all_pass() {
        0
    } else if !report.none_failed() {
        1
    } else {
        3
    };
    std::process::exit(code);
}
