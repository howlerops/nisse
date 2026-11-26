//! `nisse-deadman`: the dead-man check on the engine's heartbeat.
//!
//! Run it from cron on a host other than the engine's. A non-zero exit is the alert; how that
//! alert is delivered is up to the cron wrapper. It fails closed: any error (no database, an
//! unreadable limits file) exits non-zero too.
//!
//! Usage: `nisse-deadman <limits.json> [--source NAME]` (default source `engine`). Reads
//! `DATABASE_URL`, which is never printed (it may hold a password). Exits 0 when alive, 1 when
//! not, 2 on bad arguments. The logic is in `nisse_eval::deadman`.

use std::path::Path;

use nisse_core::{Clock, SystemClock};
use nisse_eval::deadman::{Liveness, check};
use nisse_risk::Limits;

const USAGE: &str = "usage: nisse-deadman <limits.json> [--source NAME]";

/// The limits path and the source name (default `engine`), or `None` for other arguments.
fn parse_args(args: &[String]) -> Option<(&str, &str)> {
    let mut path = None;
    let mut source = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--source" {
            if source.is_some() {
                return None;
            }
            source = Some(iter.next()?.as_str());
        } else if path.is_none() && !arg.starts_with('-') {
            path = Some(arg.as_str());
        } else {
            return None;
        }
    }
    Some((path?, source.unwrap_or("engine")))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some((path, source)) = parse_args(&args) else {
        eprintln!("{USAGE}");
        std::process::exit(2);
    };

    let limits = Limits::load(Path::new(path))?;
    let max_age = limits.heartbeat_max_age_seconds;
    let url =
        std::env::var("DATABASE_URL").map_err(|_| anyhow::anyhow!("DATABASE_URL is not set"))?;
    // Deliberately not printed: the URL may contain a password.
    let client = nisse_store::connect(&url).await?;

    let liveness = check(&client, source, SystemClock.now(), max_age).await?;
    let (line, alive) = match liveness {
        Liveness::Alive { age_seconds } => (format!("alive: {source}, {age_seconds} s"), true),
        Liveness::Stale { age_seconds } => (
            format!("STALE: {source}, last heartbeat {age_seconds} s ago (limit {max_age} s)"),
            false,
        ),
        Liveness::Never => (format!("NEVER: {source} has no heartbeat"), false),
        Liveness::ClockSkew { ahead_seconds } => (
            format!("CLOCK SKEW: {source}, heartbeat {ahead_seconds} s in the future"),
            false,
        ),
    };
    println!("{line}");
    if alive {
        Ok(())
    } else {
        std::process::exit(1);
    }
}
