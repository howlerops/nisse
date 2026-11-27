//! The engine process: short-lived commands run from cron (`nisse-engine <command>`), each of
//! which rebuilds its state from the store and the bar files, does one thing and exits.
//!
//! ```text
//! nisse-engine session-close <config.json>
//! nisse-engine tick <config.json>
//! nisse-engine status <config.json>
//! nisse-engine kill <config.json> --by NAME
//! nisse-engine release-kill <config.json> --by NAME
//! ```
//!
//! Environment variables (none of them is ever printed):
//! - `DATABASE_URL`: the Postgres store. Read by every command.
//! - `APCA_API_KEY_ID` and `APCA_API_SECRET_KEY`: the Alpaca paper keys. Read by `session-close`,
//!   `tick` and `kill` only; `status` and `release-kill` need neither.
//!
//! Exit codes: 0 done or nothing to do; 1 failed (re-run); 2 bad arguments or config; 3 another
//! engine holds the lock. A `kill` that could not confirm the engine is idle exits 1.

use std::process::ExitCode;
use std::time::Duration;

use nisse_alpaca_adapter::{AlpacaConfig, AlpacaHttp, AlpacaTrading, Credentials};
use nisse_app::{
    AppError,
    commands::{self, Command, USAGE},
    config::EngineConfig,
    market::AlpacaMarket,
    session::{session_close, tick},
};
use nisse_core::SystemClock;
use nisse_store::{Store, StoreError, StoreWriter, WriterConfig};

/// The HTTP timeout of every Alpaca request.
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);
/// How long `kill` waits for a run in flight to finish.
const KILL_LOCK_WAIT: Duration = Duration::from_secs(60);

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
    let config = match EngineConfig::load(command.config_path()) {
        Ok(config) => config,
        Err(err) => {
            eprintln!("nisse-engine: {err}");
            return ExitCode::from(2);
        }
    };
    match execute(&command, &config).await {
        Ok(Done::Yes) => ExitCode::SUCCESS,
        Ok(Done::EngineStillRunning) => {
            println!("kill: engaged; an engine run is still in flight; run kill again");
            ExitCode::FAILURE
        }
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

/// Whether a command finished its job.
enum Done {
    Yes,
    /// `kill` engaged the switch, but a run was still in flight when the wait ran out.
    EngineStillRunning,
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
}

/// Connects, runs the command, then waits for the record writer to commit what is queued. A
/// command error wins over a writer error; a writer error fails an otherwise successful run.
async fn execute(command: &Command, config: &EngineConfig) -> Result<Done, RunError> {
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
        (Ok(done), Ok(())) => Ok(done),
        (Err(err), _) | (Ok(_), Err(err)) => Err(err),
    }
}

/// Runs one command and prints its outcome line.
async fn dispatch(
    command: &Command,
    config: &EngineConfig,
    store: &Store,
    sink: StoreWriter,
) -> Result<Done, RunError> {
    match command {
        Command::Status { .. } => {
            let status = commands::status(config, store, &SystemClock).await?;
            println!("{status}");
            Ok(Done::Yes)
        }
        Command::ReleaseKill { by, .. } => {
            commands::release_kill(store, &SystemClock, by).await?;
            println!("release-kill: released by {by}");
            Ok(Done::Yes)
        }
        Command::Kill { by, .. } => {
            let http = alpaca_http()?;
            let alpaca = AlpacaConfig::paper(config.feed);
            let trading = AlpacaTrading::new(http, &alpaca);
            let outcome = commands::kill(store, &trading, &SystemClock, by, KILL_LOCK_WAIT).await?;
            if outcome.engine_idle {
                println!("kill: engaged; engine idle");
                Ok(Done::Yes)
            } else {
                Ok(Done::EngineStillRunning)
            }
        }
        Command::Tick { .. } => {
            let (trading, market) = alpaca_trading_and_market(config)?;
            let outcome = tick(config, &market, store, sink, trading, SystemClock).await?;
            println!("{}", commands::describe_tick(&outcome));
            Ok(Done::Yes)
        }
        Command::SessionClose { .. } => {
            let (trading, market) = alpaca_trading_and_market(config)?;
            let outcome = session_close(config, &market, store, sink, trading, SystemClock).await?;
            println!("{}", commands::describe_session_close(&outcome));
            Ok(Done::Yes)
        }
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
