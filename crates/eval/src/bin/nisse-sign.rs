//! `nisse-sign`: signs a merged research config for the paper stage.
//!
//! Usage: `nisse-sign <research/configs/name.json> [--root DIR] [--out FILE]`. `--root`
//! defaults to `.`. The key is read from `NISSE_SIGNING_KEY` and is never printed, logged or
//! put in an error. The manifest is printed as pretty JSON; with `--out` it is also written
//! to that file, through a temporary file and a rename.
//!
//! Exit codes: 0 signed, 1 refused or failed, 2 bad arguments. The logic is in
//! `nisse_eval::signing_job`.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use chrono::{DateTime, SecondsFormat};
use nisse_core::{Clock, SystemClock};
use nisse_eval::signing_job::{ENV_SIGNING_KEY, sign_config};

const USAGE: &str = "usage: nisse-sign <research/configs/name.json> [--root DIR] [--out FILE]";
const PREFIX: &str = "nisse-sign: ";
const NANOS_PER_SECOND: u64 = 1_000_000_000;

/// The parsed command line.
struct Args {
    config: String,
    root: PathBuf,
    out: Option<PathBuf>,
}

/// The arguments, or `None` for anything other than the usage.
fn parse_args(args: &[String]) -> Option<Args> {
    let mut config = None;
    let mut root = None;
    let mut out = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--root" if root.is_none() => root = Some(PathBuf::from(iter.next()?)),
            "--out" if out.is_none() => out = Some(PathBuf::from(iter.next()?)),
            _ if config.is_none() && !arg.starts_with("--") => config = Some(arg.clone()),
            _ => return None,
        }
    }
    Some(Args {
        config: config?,
        root: root.unwrap_or_else(|| PathBuf::from(".")),
        out,
    })
}

/// The current UTC time to the second, RFC 3339 with `Z`, from the injected clock.
fn signed_at_now() -> Option<String> {
    let secs = i64::try_from(SystemClock.now().as_u64() / NANOS_PER_SECOND).ok()?;
    DateTime::from_timestamp(secs, 0).map(|time| time.to_rfc3339_opts(SecondsFormat::Secs, true))
}

/// Writes `bytes` to `path` through a temporary file in the same directory, then renames it.
fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    };
    fs::create_dir_all(dir)?;
    let file_name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "output has no file name"))?;
    let mut tmp_name = file_name.to_os_string();
    tmp_name.push(format!(".tmp-{}", std::process::id()));
    let tmp = dir.join(tmp_name);

    let result = write_and_rename(&tmp, path, bytes);
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

fn write_and_rename(tmp: &Path, path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = File::create(tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(tmp, path)
}

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let Some(args) = parse_args(&argv) else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };

    // Only the variable's name is ever printed; the value is not.
    let key = match std::env::var(ENV_SIGNING_KEY) {
        Ok(key) if !key.trim().is_empty() => key,
        _ => {
            eprintln!("{PREFIX}{ENV_SIGNING_KEY} is not set or is blank");
            return ExitCode::from(1);
        }
    };
    let Some(signed_at) = signed_at_now() else {
        eprintln!("{PREFIX}the current time is out of range");
        return ExitCode::from(1);
    };

    let manifest = match sign_config(
        &args.root,
        &args.config,
        &key,
        nisse_engine::GIT_SHA,
        nisse_engine::build_is_replayable(),
        &signed_at,
    ) {
        Ok(manifest) => manifest,
        Err(e) => {
            eprintln!("{PREFIX}{e}");
            return ExitCode::from(1);
        }
    };

    let text = match serde_json::to_string_pretty(&manifest.to_json()) {
        Ok(text) => text,
        Err(e) => {
            eprintln!("{PREFIX}cannot render the manifest: {e}");
            return ExitCode::from(1);
        }
    };
    if let Some(out) = &args.out
        && let Err(e) = write_atomic(out, text.as_bytes())
    {
        eprintln!("{PREFIX}cannot write {}: {e}", out.display());
        return ExitCode::from(1);
    }
    println!("{text}");
    ExitCode::SUCCESS
}
