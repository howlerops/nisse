//! `nisse-proposal-check`: the checks a proposal pull request must pass on `research/`.
//!
//! CI runs it on every pull request. The changed paths go on standard input as the output of
//! `git diff -z --name-status` (NUL-separated, not the line format); the base and head checkouts
//! are the two directories. A pull request that changes nothing under `research/` passes
//! untouched, unless `--agent` is given: a research agent's pull request must change research
//! files, and every path is checked, not only the ones under `research/`.
//!
//! Usage: `nisse-proposal-check --base <dir> --head <dir> [--today YYYY-MM-DD] [--agent]`.
//! `--today` defaults to the current UTC date. Exit codes: 0 pass, 1 one or more failures (each
//! printed on its own line), 2 bad arguments or unreadable input. The logic is in
//! `nisse_eval::proposal`.

use std::io::Read;
use std::path::PathBuf;

use chrono::{DateTime, NaiveDate};
use nisse_core::{Clock, SystemClock};
use nisse_eval::proposal::{check_proposal, parse_changes, touches_research};

const USAGE: &str = "usage: nisse-proposal-check --base <dir> --head <dir> [--today YYYY-MM-DD] [--agent]\n\
     standard input: the output of `git diff -z --name-status`";
const PREFIX: &str = "proposal check: ";
const NANOS_PER_SECOND: u64 = 1_000_000_000;

/// The parsed command line.
struct Args {
    base: PathBuf,
    head: PathBuf,
    today: Option<NaiveDate>,
    agent: bool,
}

/// The arguments, or `None` for anything other than the usage.
fn parse_args(args: &[String]) -> Option<Args> {
    let mut base = None;
    let mut head = None;
    let mut today = None;
    let mut agent = false;
    let mut iter = args.iter();
    while let Some(flag) = iter.next() {
        if flag.as_str() == "--agent" {
            if agent {
                return None;
            }
            agent = true;
            continue;
        }
        let value = iter.next()?;
        match flag.as_str() {
            "--base" if base.is_none() => base = Some(PathBuf::from(value)),
            "--head" if head.is_none() => head = Some(PathBuf::from(value)),
            "--today" if today.is_none() => {
                today = Some(NaiveDate::parse_from_str(value, "%Y-%m-%d").ok()?);
            }
            _ => return None,
        }
    }
    Some(Args {
        base: base?,
        head: head?,
        today,
        agent,
    })
}

/// The current UTC date, from the injected clock. `None` when the time is out of range.
fn utc_today() -> Option<NaiveDate> {
    let nanos = SystemClock.now().as_u64();
    let secs = i64::try_from(nanos / NANOS_PER_SECOND).ok()?;
    DateTime::from_timestamp(secs, 0).map(|time| time.date_naive())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(args) = parse_args(&args) else {
        eprintln!("{USAGE}");
        std::process::exit(2);
    };
    if !args.base.is_dir() || !args.head.is_dir() {
        eprintln!("nisse-proposal-check: --base and --head must be directories");
        std::process::exit(2);
    }
    let Some(today) = args.today.or_else(utc_today) else {
        eprintln!("nisse-proposal-check: the current date is out of range; pass --today");
        std::process::exit(2);
    };

    let mut text = String::new();
    if let Err(e) = std::io::stdin().read_to_string(&mut text) {
        eprintln!("nisse-proposal-check: cannot read standard input: {e}");
        std::process::exit(2);
    }
    let changes = match parse_changes(&text) {
        Ok(changes) => changes,
        Err(e) => {
            eprintln!(
                "nisse-proposal-check: standard input must be `git diff -z --name-status` output: {e}"
            );
            std::process::exit(2);
        }
    };

    if !args.agent && !touches_research(&changes) {
        println!("{PREFIX}ok (no research change)");
        return;
    }
    let failures = check_proposal(&changes, args.agent, &args.base, &args.head, today);
    if failures.is_empty() {
        println!("{PREFIX}ok");
        return;
    }
    for failure in &failures {
        println!("{PREFIX}{failure}");
    }
    std::process::exit(1);
}
