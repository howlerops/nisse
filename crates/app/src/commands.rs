//! The `nisse-engine` command line: argument parsing and the `kill`, `release-kill` and
//! `status` commands. `tick` and `session-close` are in [`crate::session`].

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, SecondsFormat};
use nisse_bars::BarReader;
use nisse_core::nautilus::UnixNanos;
use nisse_core::{Broker, Clock, PositionEvent, RiskEvent, RiskEventKind};
use nisse_engine::{EngineError, RebalanceReport, SyncOptions};
use nisse_risk::{DrawdownMode, RiskState};
use nisse_store::{
    Store,
    tables::{
        engine_step::StepStatus,
        heartbeat,
        scoring::{self, ENGINE_LOCK},
    },
};
use rust_decimal::Decimal;
use serde_json::json;

use crate::AppError;
use crate::config::EngineConfig;
use crate::session::{
    RESIZE_RUN_KEY, RESIZE_STEP, RebalanceStep, SessionCloseOutcome, TickOutcome,
};

/// The usage text printed with an argument error.
pub const USAGE: &str = "usage: nisse-engine <session-close|tick|status> <config.json>\n       nisse-engine <kill|release-kill> <config.json> --by NAME";

/// The longest name `--by` accepts.
const MAX_BY_CHARS: usize = 64;
/// How often `kill` retries the engine lock while it waits.
const LOCK_RETRY: Duration = Duration::from_millis(500);

/// One parsed command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    SessionClose { config: PathBuf },
    Tick { config: PathBuf },
    Kill { config: PathBuf, by: String },
    ReleaseKill { config: PathBuf, by: String },
    Status { config: PathBuf },
}

impl Command {
    /// Parses the arguments after the program name. `--by` is required for kill and
    /// release-kill (non-empty, at most 64 chars) and refused for the others; it may come
    /// before or after the config path. Anything else is `Err` with a short message.
    pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Result<Self, String> {
        let mut words = Vec::with_capacity(2);
        let mut by: Option<String> = None;
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            if arg == "--by" {
                if by.is_some() {
                    return Err("--by given more than once".to_owned());
                }
                by = Some(args.next().ok_or_else(|| "--by needs a name".to_owned())?);
            } else if arg.starts_with('-') {
                return Err(format!("unknown option `{arg}`"));
            } else {
                words.push(arg);
            }
        }

        let mut words = words.into_iter();
        let name = words.next().ok_or_else(|| "missing command".to_owned())?;
        let config = PathBuf::from(
            words
                .next()
                .ok_or_else(|| "missing config path".to_owned())?,
        );
        if let Some(extra) = words.next() {
            return Err(format!("unexpected argument `{extra}`"));
        }

        match name.as_str() {
            "kill" => Ok(Self::Kill {
                config,
                by: require_by(by)?,
            }),
            "release-kill" => Ok(Self::ReleaseKill {
                config,
                by: require_by(by)?,
            }),
            "session-close" | "tick" | "status" => {
                if by.is_some() {
                    return Err("--by is only for kill and release-kill".to_owned());
                }
                Ok(match name.as_str() {
                    "session-close" => Self::SessionClose { config },
                    "tick" => Self::Tick { config },
                    _ => Self::Status { config },
                })
            }
            other => Err(format!("unknown command `{other}`")),
        }
    }

    /// The config file the command reads.
    #[must_use]
    pub fn config_path(&self) -> &Path {
        match self {
            Self::SessionClose { config }
            | Self::Tick { config }
            | Self::Kill { config, .. }
            | Self::ReleaseKill { config, .. }
            | Self::Status { config } => config,
        }
    }
}

/// The `--by` name of a kill or release: present, not blank, at most `MAX_BY_CHARS` characters.
fn require_by(by: Option<String>) -> Result<String, String> {
    let by = by.ok_or_else(|| "--by NAME is required".to_owned())?;
    if by.trim().is_empty() {
        return Err("--by needs a non-empty name".to_owned());
    }
    if by.chars().count() > MAX_BY_CHARS {
        return Err(format!(
            "--by name is longer than {MAX_BY_CHARS} characters"
        ));
    }
    Ok(by)
}

/// What `kill` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KillOutcome {
    /// No engine process was running by the end (the engine lock could be taken), and open
    /// orders were cancelled once more after that. `false`: an engine run was still in
    /// flight when the wait ran out; run the command again.
    pub engine_idle: bool,
}

/// Engages the kill switch. Deliberately does not restore a trader and does not need the
/// engine lock first: it must work when the risk state is unreadable or a run is in flight.
///
/// 1. Record a `kill_switch` risk event (`engaged: true`, `by`) with `store.record_risk_event`.
/// 2. `broker.cancel_all()`.
/// 3. Try to take the engine lock, retrying every 500 ms until `lock_wait` has passed
///    (tokio::time::sleep). Once taken, no engine run is in flight and any later run will read
///    the switch as engaged: call `cancel_all()` again and return `engine_idle: true`.
///    Not taken in time: `engine_idle: false`.
///
/// Step 2 is attempted even when step 1 failed; if both fail the store error is returned.
pub async fn kill<B: Broker, C: Clock>(
    store: &Store,
    broker: &B,
    clock: &C,
    by: &str,
    lock_wait: Duration,
) -> Result<KillOutcome, AppError> {
    let recorded = engage_kill_switch(store, clock, by).await;
    let cancelled = cancel_open_orders(broker).await;

    let engine_idle = wait_for_engine_lock(store, lock_wait).await?;
    // The lock is held by this connection from here on, so no run is in flight. Cancel what
    // a run placed before it could see the switch. The lock goes with the process.
    let final_cancelled = if engine_idle {
        cancel_open_orders(broker).await
    } else {
        Ok(())
    };

    recorded?;
    cancelled?;
    final_cancelled?;
    Ok(KillOutcome { engine_idle })
}

/// Records the kill switch as engaged: the first half of [`kill`], on its own for when the
/// broker cannot be reached. Every engine run reads the switch when it starts.
pub async fn engage_kill_switch<C: Clock>(
    store: &Store,
    clock: &C,
    by: &str,
) -> Result<(), AppError> {
    let event = RiskEvent {
        kind: RiskEventKind::KillSwitch,
        detail: json!({ "engaged": true, "by": by }),
        ts: clock.now(),
    };
    store.record_risk_event(&event).await?;
    Ok(())
}

/// Asks the broker to cancel every open order: the second half of [`kill`], on its own for
/// when the store cannot be reached.
pub async fn cancel_open_orders<B: Broker>(broker: &B) -> Result<(), AppError> {
    broker
        .cancel_all()
        .await
        .map_err(|err| EngineError::Broker(Box::new(err)))?;
    Ok(())
}

/// Tries the engine lock every `LOCK_RETRY` until `lock_wait` has passed. `true` when taken.
async fn wait_for_engine_lock(store: &Store, lock_wait: Duration) -> Result<bool, AppError> {
    let deadline = tokio::time::Instant::now() + lock_wait;
    loop {
        if scoring::try_advisory_lock(store.client(), ENGINE_LOCK).await? {
            return Ok(true);
        }
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            return Ok(false);
        }
        tokio::time::sleep(left.min(LOCK_RETRY)).await;
    }
}

/// Releases the kill switch: one `kill_switch` risk event with `engaged: false`. Nothing else.
pub async fn release_kill<C: Clock>(store: &Store, clock: &C, by: &str) -> Result<(), AppError> {
    let event = RiskEvent {
        kind: RiskEventKind::KillSwitch,
        detail: json!({ "engaged": false, "by": by }),
        ts: clock.now(),
    };
    store.record_risk_event(&event).await?;
    Ok(())
}

/// What the engine would start from. Read from the store and the bar directory only.
#[derive(Debug, Clone, PartialEq)]
pub struct Status {
    pub kill_switch: bool,
    pub drawdown_mode: DrawdownMode,
    pub equity_peak: Option<Decimal>,
    pub resize_owed: bool,
    pub limits_hash: Option<String>,
    pub positions: Vec<PositionEvent>,
    pub live_orders: usize,
    pub last_heartbeat: Option<UnixNanos>,
    pub newest_bar_file: Option<String>,
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut lines = vec![
            format!(
                "kill switch: {}",
                if self.kill_switch { "engaged" } else { "off" }
            ),
            format!("drawdown mode: {}", self.drawdown_mode.as_str()),
            format!(
                "equity peak: {}",
                self.equity_peak
                    .map_or_else(|| "none".to_owned(), |peak| peak.to_string())
            ),
            format!("resize owed: {}", yes_no(self.resize_owed)),
            format!(
                "limits hash: {}",
                self.limits_hash.as_deref().unwrap_or("none")
            ),
            format!("live orders: {}", self.live_orders),
            format!(
                "last heartbeat: {}",
                self.last_heartbeat
                    .map_or_else(|| "none".to_owned(), time_text)
            ),
            format!(
                "newest bar file: {}",
                self.newest_bar_file.as_deref().unwrap_or("none")
            ),
        ];
        if self.positions.is_empty() {
            lines.push("positions: none".to_owned());
        } else {
            lines.push("positions:".to_owned());
            for position in &self.positions {
                lines.push(format!(
                    "  {} {} @ {}",
                    position.symbol.as_str(),
                    position.qty_after,
                    position.avg_cost
                ));
            }
        }
        f.write_str(&lines.join("\n"))
    }
}

fn yes_no(flag: bool) -> &'static str {
    if flag { "yes" } else { "no" }
}

/// `ts` as an RFC 3339 UTC time to the second, or the raw nanoseconds when out of range.
fn time_text(ts: UnixNanos) -> String {
    let nanos = ts.as_u64();
    let secs = i64::try_from(nanos / 1_000_000_000).ok();
    let subsec = u32::try_from(nanos % 1_000_000_000).ok();
    secs.zip(subsec)
        .and_then(|(secs, subsec)| DateTime::from_timestamp(secs, subsec))
        .map_or_else(
            || nanos.to_string(),
            |time| time.to_rfc3339_opts(SecondsFormat::Secs, true),
        )
}

/// Reads the status. Writes nothing (no strategy version, no limits event), takes no lock,
/// talks to no broker.
pub async fn status<C: Clock>(
    config: &EngineConfig,
    store: &Store,
    clock: &C,
) -> Result<Status, AppError> {
    let now = clock.now();
    let risk = RiskState::restore(&store.load_risk_events().await?)?;
    let resize_owed = matches!(
        store.step_status(RESIZE_RUN_KEY, RESIZE_STEP).await?,
        Some((StepStatus::Started, _))
    );
    let recheck_since = UnixNanos::from(
        now.as_u64()
            .saturating_sub(SyncOptions::default().never_arrived_recheck_nanos),
    );
    let live_orders = store.load_live_orders(Some(recheck_since)).await?.len();
    let newest_bar_file = if config.bars_dir.is_dir() {
        BarReader::open(&config.bars_dir)?.snapshot_file_name()?
    } else {
        None
    };
    Ok(Status {
        kill_switch: risk.kill_switch_engaged,
        drawdown_mode: risk.drawdown.mode,
        equity_peak: risk.drawdown.peak,
        resize_owed,
        limits_hash: store.latest_limits_hash().await?,
        positions: store.load_positions().await?,
        live_orders,
        last_heartbeat: heartbeat::latest(store.client(), &config.source).await?,
        newest_bar_file,
    })
}

/// `count` and `noun`, with `s` added unless `count` is one.
fn counted(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("1 {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

/// `drawdown mode from -> to`, when the mode changed.
fn mode_change_text(change: Option<(DrawdownMode, DrawdownMode)>) -> Option<String> {
    change.map(|(from, to)| format!("drawdown mode {} -> {}", from.as_str(), to.as_str()))
}

/// `N orders sent, F failed, B blocked` for a rebalance.
fn rebalance_text(report: &RebalanceReport) -> String {
    format!(
        "rebalance: {} sent, {} failed, {} blocked",
        counted(report.orders_submitted as usize, "order"),
        report.orders_failed,
        report.orders_blocked
    )
}

/// The one line `tick` prints, e.g. `tick: 2 fills; market open; no rebalance`.
#[must_use]
pub fn describe_tick(outcome: &TickOutcome) -> String {
    let mut parts = vec![
        counted(outcome.fills, "fill"),
        if outcome.market_open {
            "market open".to_owned()
        } else {
            "market closed".to_owned()
        },
        match &outcome.rebalance {
            Some(report) => rebalance_text(report),
            None => "no rebalance".to_owned(),
        },
    ];
    parts.extend(mode_change_text(outcome.mode_change));
    format!("tick: {}", parts.join("; "))
}

/// The one line `session-close` prints, e.g.
/// `session-close 2026-01-05: ingest wrote 21000 bars; 0 fills; rebalance not due`.
#[must_use]
pub fn describe_session_close(outcome: &SessionCloseOutcome) -> String {
    match outcome {
        SessionCloseOutcome::NoSession => {
            "session-close: no session closed in the last 6 hours; nothing done".to_owned()
        }
        SessionCloseOutcome::Ran {
            session_date,
            ingest,
            fills,
            mode_change,
            rebalance,
        } => {
            let ingest_text = if ingest.skipped {
                "ingest skipped; bar file already present".to_owned()
            } else {
                format!("ingest wrote {}", counted(ingest.bars, "bar"))
            };
            let rebalance_text = match rebalance {
                RebalanceStep::NotDue => "rebalance not due".to_owned(),
                RebalanceStep::AlreadyDone => "rebalance already done".to_owned(),
                RebalanceStep::Halted => {
                    "rebalance due but trading is halted; it is owed".to_owned()
                }
                RebalanceStep::Ran(report) => rebalance_text(report),
            };
            let mut parts = vec![ingest_text, counted(*fills, "fill"), rebalance_text];
            parts.extend(mode_change_text(*mode_change));
            format!("session-close {session_date}: {}", parts.join("; "))
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use nisse_core::nautilus::Symbol;

    fn args(words: &[&str]) -> Vec<String> {
        words.iter().map(|word| (*word).to_owned()).collect()
    }

    fn parse(words: &[&str]) -> Result<Command, String> {
        Command::parse(args(words))
    }

    #[test]
    fn each_command_parses() {
        assert_eq!(
            parse(&["session-close", "c.json"]),
            Ok(Command::SessionClose {
                config: "c.json".into()
            })
        );
        assert_eq!(
            parse(&["tick", "c.json"]),
            Ok(Command::Tick {
                config: "c.json".into()
            })
        );
        assert_eq!(
            parse(&["status", "c.json"]),
            Ok(Command::Status {
                config: "c.json".into()
            })
        );
        assert_eq!(
            parse(&["kill", "c.json", "--by", "ana"]),
            Ok(Command::Kill {
                config: "c.json".into(),
                by: "ana".to_owned()
            })
        );
        assert_eq!(
            parse(&["release-kill", "c.json", "--by", "ana"]),
            Ok(Command::ReleaseKill {
                config: "c.json".into(),
                by: "ana".to_owned()
            })
        );
    }

    #[test]
    fn by_may_come_before_the_path() {
        assert_eq!(
            parse(&["kill", "--by", "ana", "c.json"]),
            Ok(Command::Kill {
                config: "c.json".into(),
                by: "ana".to_owned()
            })
        );
        assert_eq!(
            parse(&["--by", "ana", "release-kill", "c.json"]),
            Ok(Command::ReleaseKill {
                config: "c.json".into(),
                by: "ana".to_owned()
            })
        );
    }

    #[test]
    fn kill_without_by_is_refused() {
        assert!(parse(&["kill", "c.json"]).is_err());
        assert!(parse(&["release-kill", "c.json"]).is_err());
    }

    #[test]
    fn by_for_other_commands_is_refused() {
        assert!(parse(&["tick", "c.json", "--by", "ana"]).is_err());
        assert!(parse(&["status", "--by", "ana", "c.json"]).is_err());
        assert!(parse(&["session-close", "c.json", "--by", "ana"]).is_err());
    }

    #[test]
    fn bad_command_lines_are_refused() {
        assert!(parse(&["frobnicate", "c.json"]).is_err());
        assert!(parse(&["tick"]).is_err());
        assert!(parse(&[]).is_err());
        assert!(parse(&["tick", "c.json", "extra"]).is_err());
        assert!(parse(&["kill", "c.json", "--by", "a", "--by", "b"]).is_err());
        assert!(parse(&["kill", "c.json", "--by"]).is_err());
        assert!(parse(&["tick", "c.json", "--force"]).is_err());
    }

    #[test]
    fn empty_and_long_by_names_are_refused() {
        assert!(parse(&["kill", "c.json", "--by", ""]).is_err());
        assert!(parse(&["kill", "c.json", "--by", "   "]).is_err());
        let long = "x".repeat(MAX_BY_CHARS + 1);
        assert!(parse(&["kill", "c.json", "--by", &long]).is_err());
        let exact = "x".repeat(MAX_BY_CHARS);
        assert!(parse(&["kill", "c.json", "--by", &exact]).is_ok());
    }

    #[test]
    fn config_path_is_the_path_of_every_variant() {
        let commands = [
            parse(&["session-close", "a.json"]),
            parse(&["tick", "a.json"]),
            parse(&["kill", "a.json", "--by", "x"]),
            parse(&["release-kill", "a.json", "--by", "x"]),
            parse(&["status", "a.json"]),
        ];
        for command in commands {
            let command = command.unwrap();
            assert_eq!(command.config_path(), Path::new("a.json"));
        }
    }

    fn session(day: u32) -> chrono::NaiveDate {
        chrono::NaiveDate::from_ymd_opt(2026, 1, day).unwrap()
    }

    #[test]
    fn describe_tick_lines() {
        let open = TickOutcome {
            fills: 2,
            mode_change: None,
            market_open: true,
            rebalance: None,
        };
        assert_eq!(
            describe_tick(&open),
            "tick: 2 fills; market open; no rebalance"
        );

        let one_closed = TickOutcome {
            fills: 1,
            mode_change: None,
            market_open: false,
            rebalance: None,
        };
        assert_eq!(
            describe_tick(&one_closed),
            "tick: 1 fill; market closed; no rebalance"
        );

        let rebalanced = TickOutcome {
            fills: 0,
            mode_change: Some((DrawdownMode::Normal, DrawdownMode::Halved)),
            market_open: true,
            rebalance: Some(RebalanceReport {
                decisions: Vec::new(),
                orders_submitted: 3,
                orders_failed: 1,
                orders_blocked: 0,
            }),
        };
        assert_eq!(
            describe_tick(&rebalanced),
            "tick: 0 fills; market open; rebalance: 3 orders sent, 1 failed, 0 blocked; \
             drawdown mode normal -> halved"
        );
    }

    #[test]
    fn describe_session_close_lines() {
        let no_session = SessionCloseOutcome::NoSession;
        assert_eq!(
            describe_session_close(&no_session),
            "session-close: no session closed in the last 6 hours; nothing done"
        );

        let ran = SessionCloseOutcome::Ran {
            session_date: session(5),
            ingest: crate::ingest::IngestOutcome {
                path: PathBuf::from("bars/2026-01-05.parquet"),
                bars: 21_000,
                skipped: false,
            },
            fills: 0,
            mode_change: None,
            rebalance: RebalanceStep::NotDue,
        };
        assert_eq!(
            describe_session_close(&ran),
            "session-close 2026-01-05: ingest wrote 21000 bars; 0 fills; rebalance not due"
        );

        let skipped = SessionCloseOutcome::Ran {
            session_date: session(5),
            ingest: crate::ingest::IngestOutcome {
                path: PathBuf::from("bars/2026-01-05.parquet"),
                bars: 0,
                skipped: true,
            },
            fills: 1,
            mode_change: None,
            rebalance: RebalanceStep::AlreadyDone,
        };
        assert_eq!(
            describe_session_close(&skipped),
            "session-close 2026-01-05: ingest skipped; bar file already present; 1 fill; \
             rebalance already done"
        );

        let rebalanced = SessionCloseOutcome::Ran {
            session_date: session(30),
            ingest: crate::ingest::IngestOutcome {
                path: PathBuf::from("bars/2026-01-30.parquet"),
                bars: 1,
                skipped: false,
            },
            fills: 0,
            mode_change: Some((DrawdownMode::Halved, DrawdownMode::Flat)),
            rebalance: RebalanceStep::Ran(RebalanceReport {
                decisions: Vec::new(),
                orders_submitted: 1,
                orders_failed: 0,
                orders_blocked: 2,
            }),
        };
        assert_eq!(
            describe_session_close(&rebalanced),
            "session-close 2026-01-30: ingest wrote 1 bar; 0 fills; \
             rebalance: 1 order sent, 0 failed, 2 blocked; drawdown mode halved -> flat"
        );
    }

    #[test]
    fn status_display_lines() {
        let status = Status {
            kill_switch: true,
            drawdown_mode: DrawdownMode::Normal,
            equity_peak: Some(Decimal::from(100_000)),
            resize_owed: false,
            limits_hash: None,
            positions: vec![PositionEvent {
                symbol: Symbol::new_checked("SPY").unwrap(),
                qty_after: Decimal::from(10),
                avg_cost: Decimal::new(45_000, 2),
                ts: UnixNanos::from(0_u64),
            }],
            live_orders: 2,
            last_heartbeat: Some(UnixNanos::from(1_767_625_200_000_000_000_u64)),
            newest_bar_file: Some("2026-01-05.parquet".to_owned()),
        };
        let text = status.to_string();
        assert_eq!(
            text,
            "kill switch: engaged\n\
             drawdown mode: normal\n\
             equity peak: 100000\n\
             resize owed: no\n\
             limits hash: none\n\
             live orders: 2\n\
             last heartbeat: 2026-01-05T15:00:00Z\n\
             newest bar file: 2026-01-05.parquet\n\
             positions:\n\
             \x20 SPY 10 @ 450.00"
        );
    }
}
