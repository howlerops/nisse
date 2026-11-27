//! `nisse-agent-report`: the aggregates-only report the research agents read. Run it nightly
//! from cron, after the scoring job.
//!
//! Usage: `nisse-agent-report <gates.json> <research-config.json> <bars_dir> <out.json>`.
//! The research config is read only for its `symbols` and `benchmark`. Reads `DATABASE_URL`
//! (never printed). Writes the report pretty-printed to `<out.json>` through a temporary file
//! in the same directory and a rename, so a reader never sees half a file. Exits 2 on bad
//! arguments and 1 on any error.

use std::io::Write;
use std::path::Path;

use nisse_core::{Clock, SystemClock};
use nisse_eval::agent_report::{ReportArgs, build_agent_report};
use nisse_eval::gates::Gates;
use serde_json::Value;

const USAGE: &str = "usage: nisse-agent-report <gates.json> <research-config.json> <bars_dir> \
                     <out.json>";

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [gates_path, config_path, bars_dir, out_path] = args.as_slice() else {
        eprintln!("{USAGE}");
        std::process::exit(2);
    };
    if let Err(err) = run(gates_path, config_path, bars_dir, out_path).await {
        eprintln!("nisse-agent-report: {err:#}");
        std::process::exit(1);
    }
}

async fn run(
    gates_path: &str,
    config_path: &str,
    bars_dir: &str,
    out_path: &str,
) -> anyhow::Result<()> {
    let gates = Gates::load(Path::new(gates_path))?;
    let config: Value = serde_json::from_str(&std::fs::read_to_string(config_path)?)?;
    let symbols = config_symbols(&config)?;
    let benchmark = config
        .get("benchmark")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("{config_path}: benchmark is missing or not a string"))?;

    // Deliberately not printed: the URL may contain a password.
    let url =
        std::env::var("DATABASE_URL").map_err(|_| anyhow::anyhow!("DATABASE_URL is not set"))?;
    let client = nisse_store::connect(&url).await?;

    let report = build_agent_report(
        &client,
        &ReportArgs {
            gates: &gates,
            bars_dir: Path::new(bars_dir),
            symbols: &symbols,
            benchmark,
            now: SystemClock.now(),
        },
    )
    .await?;
    let text = serde_json::to_string_pretty(&report.to_json())?;
    write_atomically(Path::new(out_path), &text)?;
    Ok(())
}

/// The `symbols` of a research config: an array of strings.
fn config_symbols(config: &Value) -> anyhow::Result<Vec<String>> {
    let list = config
        .get("symbols")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("research config: symbols is missing or not an array"))?;
    list.iter()
        .map(|symbol| {
            symbol
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| anyhow::anyhow!("research config: a symbol is not a string"))
        })
        .collect()
}

/// Writes `text` to `path` through a temporary file beside it, then renames it into place.
fn write_atomically(path: &Path, text: &str) -> anyhow::Result<()> {
    let name = path
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("output path has no file name"))?;
    let mut temp_name = std::ffi::OsString::from(".");
    temp_name.push(name);
    temp_name.push(format!(".{}.tmp", std::process::id()));
    let temp = path.with_file_name(temp_name);

    let written = (|| -> std::io::Result<()> {
        let mut file = std::fs::File::create(&temp)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&temp, path)
    })();
    if let Err(err) = written {
        // Best effort: the temporary file is never read, so a failure to remove it is harmless.
        let _ = std::fs::remove_file(&temp);
        return Err(err.into());
    }
    Ok(())
}
