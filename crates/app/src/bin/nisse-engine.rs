//! The engine process: short-lived commands run from cron (`nisse-engine <command>`), each of
//! which rebuilds its state from the store and the bar files, does one thing and exits.
//!
//! ```text
//! nisse-engine session-close <config.json>
//! nisse-engine tick <config.json>
//! nisse-engine status <config.json>
//! nisse-engine kill-watch <config.json>
//! nisse-engine kill <config.json> --by NAME
//! nisse-engine release-kill <config.json> --by NAME
//! ```
//!
//! Environment variables (none of them is ever printed):
//! - `DATABASE_URL`: the Postgres store. Read by every command.
//! - `APCA_API_KEY_ID` and `APCA_API_SECRET_KEY`: the Alpaca paper keys. Read by `session-close`,
//!   `tick`, `kill` and `kill-watch` only (`kill-watch` only when the switch is engaged);
//!   `status` and `release-kill` need neither.
//! - `NISSE_SIGNING_KEY`: the signing key as hex. Read by `session-close` and `tick` only, and
//!   only when the config requires a signature (`signature.required`).
//!
//! `kill-watch` runs every minute: with the switch off it reads one row and exits; engaged, it
//! cancels what rests at the broker (see `kill_watch`). It takes no engine lock and does not
//! need a valid config beyond `limits_path` (a broken config still enforces, as a halt).
//!
//! Exit codes: 0 done or nothing to do; 1 failed (re-run); 2 bad arguments or config, or a
//! deployment that is not signed when its config requires it (nothing is traded); 3 another
//! engine holds the lock. A `kill` that could not confirm the engine is idle exits 1.

use std::process::ExitCode;
use std::time::Duration;

use nisse_alpaca_adapter::{AlpacaConfig, AlpacaHttp, AlpacaTrading, Credentials};
use nisse_app::{
    AppError,
    commands::{self, Command, USAGE},
    config::EngineConfig,
    deployment::{Deployment, ENV_SIGNING_KEY, verify_deployment},
    kill_watch::{flatten_on_kill, watch_once},
    market::AlpacaMarket,
    restore::wait_for_engine_lock,
    session::{session_close, tick},
};
use nisse_core::{Clock, SystemClock};
use nisse_store::{Store, StoreError, StoreWriter, WriterConfig};

/// The HTTP timeout of every Alpaca request.
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);
/// How long `kill` waits for a run in flight to finish.
const KILL_LOCK_WAIT: Duration = Duration::from_secs(60);
/// How long `tick` and `session-close` wait for another engine command to finish.
const ENGINE_LOCK_WAIT: Duration = Duration::from_secs(45);

#[tokio::main]
async fn main() -> ExitCode {
    let command = match Command::parse(std::env::args().skip(1)) {
        Ok(command) => command,
        Err(message) => {
            eprintln!("nisse-engine: {message}");
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    // `kill` must work when the config file is broken, so it never reads it: the trading
    // client needs nothing from it (this build only knows Alpaca's paper endpoints).
    if let Command::Kill { by, .. } = &command {
        return run_kill(by).await;
    }
    if let Command::KillWatch { config } = &command {
        return run_kill_watch(config).await;
    }
    let config = match EngineConfig::load(command.config_path()) {
        Ok(config) => config,
        Err(err) => {
            eprintln!("nisse-engine: {err}");
            return ExitCode::from(2);
        }
    };
    // Trading commands refuse a deployment that its config requires to be signed, before any
    // client or store connection is made.
    if matches!(command, Command::Tick { .. } | Command::SessionClose { .. }) {
        let key = std::env::var(ENV_SIGNING_KEY).ok();
        match verify_deployment(&config, key.as_deref()) {
            Ok(deployment) => println!("{}", describe_deployment(&deployment)),
            Err(err) => {
                eprintln!("nisse-engine: {err}");
                return ExitCode::from(2);
            }
        }
    }
    match execute(&command, &config).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(RunError::App(AppError::Busy)) => {
            eprintln!("nisse-engine: another engine process is running");
            ExitCode::from(3)
        }
        Err(err) => {
            eprintln!("nisse-engine: {err}");
            ExitCode::FAILURE
        }
    }
}

/// The line printed before a trading command runs, saying what was checked.
fn describe_deployment(deployment: &Deployment) -> String {
    match deployment {
        Deployment::Signed {
            name,
            git_sha,
            deployment_hash,
        } => format!("deployment: signed {name} at {git_sha} ({deployment_hash})"),
        Deployment::Unsigned { deployment_hash } => format!(
            "deployment: UNSIGNED ({deployment_hash}); set \"required\": true once a key exists"
        ),
    }
}

/// Why a command failed: the store, the application, or the record writer.
#[derive(Debug, thiserror::Error)]
enum RunError {
    #[error("{0} is not set")]
    Env(&'static str),
    #[error(transparent)]
    App(#[from] AppError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("record writer task failed: {0}")]
    WriterTask(String),
    #[error("kill and kill-watch are handled before the other commands")]
    KillNotDispatched,
}

/// Connects, runs the command, then waits for the record writer to commit what is queued. A
/// command error wins over a writer error; a writer error fails an otherwise successful run.
async fn execute(command: &Command, config: &EngineConfig) -> Result<(), RunError> {
    let url = std::env::var("DATABASE_URL").map_err(|_| RunError::Env("DATABASE_URL"))?;
    // Deliberately not printed: the URL may contain a password.
    let store = Store::new(nisse_store::connect(&url).await?);
    let (sink, writer) =
        StoreWriter::spawn(nisse_store::connect(&url).await?, WriterConfig::default());

    // `sink` is moved in, so once the command returns the writer has no handles left.
    let result = dispatch(command, config, &store, sink).await;
    let written = match writer.await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(err)) => Err(RunError::Store(err)),
        Err(err) => Err(RunError::WriterTask(err.to_string())),
    };
    match (result, written) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(err), _) | (Ok(_), Err(err)) => Err(err),
    }
}

/// `kill`. The library's `kill` cancels at the broker while it connects to the store and
/// records the switch, waits for a run in flight, cancels once more and checks that no order
/// is left open. Whatever could not be done is printed, one line each, and makes the exit
/// code 1.
async fn run_kill(by: &str) -> ExitCode {
    // The feed only matters to market data, which `kill` never reads.
    let trading = alpaca_http().map(|http| {
        AlpacaTrading::new(http, &AlpacaConfig::paper(nisse_alpaca_adapter::Feed::Iex))
    });
    // Said at once: the rest can take a minute, and this is when to go out of band.
    if let Err(err) = &trading {
        eprintln!("nisse-engine: kill: no broker client, open orders will NOT be cancelled: {err}");
    }
    let connect = async { connect_store().await.map_err(|err| err.to_string()) };
    let outcome = commands::kill(
        connect,
        trading.as_ref().ok(),
        &SystemClock,
        by,
        KILL_LOCK_WAIT,
    )
    .await;
    for line in &outcome.errors {
        eprintln!("nisse-engine: kill: {line}");
    }
    println!(
        "kill: switch {}; orders {}; engine {}",
        if outcome.switch_recorded {
            "engaged"
        } else {
            "NOT recorded"
        },
        if outcome.orders_cancelled {
            "cancelled"
        } else {
            "NOT cancelled"
        },
        if outcome.engine_idle {
            "idle"
        } else {
            "possibly still running"
        },
    );
    if outcome.complete() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// `kill-watch`. Reads the switch from the store first: off, it prints that and exits 0 with
/// no broker client (missing Alpaca keys are no error then). Engaged, it builds the trading
/// client and enforces the switch with it. No engine lock, no signature check, no record
/// writer. The config is read only for `limits_path`; if it cannot be read, the switch halts
/// (no flatten) and everything is cancelled, and that is said on stderr.
async fn run_kill_watch(config_path: &std::path::Path) -> ExitCode {
    let flatten = match EngineConfig::load(config_path) {
        Ok(config) => flatten_on_kill(&config.limits_path),
        Err(err) => {
            eprintln!(
                "nisse-engine: kill-watch: config unreadable, using flatten_on_kill = false: {err}"
            );
            false
        }
    };
    let store = match connect_store().await {
        Ok(store) => store,
        Err(err) => {
            eprintln!("nisse-engine: kill-watch: {err}");
            return ExitCode::FAILURE;
        }
    };
    // The broker client is only needed when the switch is engaged; missing keys are not an
    // error while it is off. `watch_once` records the failure when they are needed and absent.
    let trading = match alpaca_http() {
        Ok(http) => Some(AlpacaTrading::new(
            http,
            &AlpacaConfig::paper(nisse_alpaca_adapter::Feed::Iex),
        )),
        Err(_) => None,
    };
    match watch_once(&store, trading.as_ref(), flatten, SystemClock.now()).await {
        Ok(outcome) => {
            println!("{}", commands::describe_kill_watch(&outcome));
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("nisse-engine: kill-watch: {err}");
            ExitCode::FAILURE
        }
    }
}

/// Connects to the store named by `DATABASE_URL`, which is never printed (it may hold a
/// password).
async fn connect_store() -> Result<Store, RunError> {
    let url = std::env::var("DATABASE_URL").map_err(|_| RunError::Env("DATABASE_URL"))?;
    Ok(Store::new(nisse_store::connect(&url).await?))
}

/// Runs one command and prints its outcome line.
async fn dispatch(
    command: &Command,
    config: &EngineConfig,
    store: &Store,
    sink: StoreWriter,
) -> Result<(), RunError> {
    match command {
        Command::Status { .. } => {
            let status = commands::status(config, store, &SystemClock).await?;
            println!("{status}");
            Ok(())
        }
        Command::ReleaseKill { by, .. } => {
            commands::release_kill(store, &SystemClock, by).await?;
            println!("release-kill: released by {by}");
            Ok(())
        }
        // `main` handles `kill` and `kill-watch` before any config or store is needed; this
        // arm only keeps the match total.
        Command::Kill { .. } | Command::KillWatch { .. } => Err(RunError::KillNotDispatched),
        Command::Tick { .. } => {
            let (trading, market) = alpaca_trading_and_market(config)?;
            wait_for_turn(store).await?;
            let outcome = tick(config, &market, store, sink, trading, SystemClock).await?;
            println!("{}", commands::describe_tick(&outcome));
            Ok(())
        }
        Command::SessionClose { .. } => {
            let (trading, market) = alpaca_trading_and_market(config)?;
            wait_for_turn(store).await?;
            let outcome = session_close(config, &market, store, sink, trading, SystemClock).await?;
            println!("{}", commands::describe_session_close(&outcome));
            Ok(())
        }
    }
}

/// Waits up to `ENGINE_LOCK_WAIT` for another engine command to finish (a tick and a
/// session-close scheduled close together), then gives up as `Busy`.
async fn wait_for_turn(store: &Store) -> Result<(), AppError> {
    if wait_for_engine_lock(store, ENGINE_LOCK_WAIT).await? {
        Ok(())
    } else {
        Err(AppError::Busy)
    }
}

/// The paper HTTP client. Reads the Alpaca keys from the environment; never prints them.
fn alpaca_http() -> Result<AlpacaHttp, AppError> {
    let credentials = Credentials::from_env()?;
    Ok(AlpacaHttp::new(&credentials, HTTP_TIMEOUT)?)
}

/// The paper trading client and the market data, over one HTTP client, for the config's feed.
fn alpaca_trading_and_market(
    config: &EngineConfig,
) -> Result<(AlpacaTrading, AlpacaMarket), AppError> {
    let http = alpaca_http()?;
    let alpaca = AlpacaConfig::paper(config.feed);
    let trading = AlpacaTrading::new(http.clone(), &alpaca);
    Ok((trading, AlpacaMarket::new(http, alpaca)))
}
