//! `nisse-agent-report`: the aggregates-only report the research agents read. Run it nightly
//! from cron, after the scoring job.
//!
//! Usage: `nisse-agent-report <gates.json> <research-config.json> <bars_dir> <out.json>`.
//! The research config is parsed and validated by `ResearchConfig::parse`; only its `symbols`
//! and `benchmark` are used. Reads `DATABASE_URL`
//! (never printed). Writes the report pretty-printed to `<out.json>` through a temporary file
//! in the same directory and a rename, so a reader never sees half a file. Exits 2 on bad
//! arguments and 1 on any error.

use std::io::Write;
use std::path::Path;

use nisse_core::{Clock, SystemClock};
use nisse_eval::agent_report::{ReportArgs, build_agent_report};
use nisse_eval::gates::Gates;
use nisse_eval::proposal::ResearchConfig;

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
    let config = ResearchConfig::parse(&std::fs::read_to_string(config_path)?)
        .map_err(|err| anyhow::anyhow!("{config_path}: {err}"))?;

    // Deliberately not printed: the URL may contain a password.
    let url =
        std::env::var("DATABASE_URL").map_err(|_| anyhow::anyhow!("DATABASE_URL is not set"))?;
    let client = nisse_store::connect(&url).await?;

    let report = build_agent_report(
        &client,
        &ReportArgs {
            gates: &gates,
            bars_dir: Path::new(bars_dir),
            symbols: &config.symbols,
            benchmark: &config.benchmark,
            now: SystemClock.now(),
        },
    )
    .await?;
    let text = serde_json::to_string_pretty(&report.to_json())?;
    write_atomically(Path::new(out_path), &text)?;
    Ok(())
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
