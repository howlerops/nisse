//! Log output for the binaries: `tracing` events as plain lines on standard error, where cron's
//! redirect (`deploy/run.sh`) collects them. Libraries only emit events; a binary calls
//! [`init`] once, first thing in `main`. Logging never feeds a decision.

use tracing_subscriber::filter::LevelFilter;

/// The environment variable choosing the most verbose level written: `error`, `warn`, `info`,
/// `debug`, `trace` or `off` (any case). Unset, blank or anything else means `info`.
pub const ENV_LOG_LEVEL: &str = "NISSE_LOG";

/// The level `text` names, or `info`.
fn level(text: Option<&str>) -> LevelFilter {
    // An empty string parses as `error` in `tracing`; here it means unset.
    text.map(str::trim)
        .filter(|text| !text.is_empty())
        .and_then(|text| text.parse().ok())
        .unwrap_or(LevelFilter::INFO)
}

/// Installs the process's log subscriber: one line per event, UTC timestamp, no colour codes.
/// A second call, or a subscriber already installed by a test harness, is left alone.
pub fn init() {
    let level = level(std::env::var(ENV_LOG_LEVEL).ok().as_deref());
    // An error only means a subscriber is already installed.
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .with_max_level(level)
        .try_init();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_parses_names_in_any_case() {
        assert_eq!(level(Some("debug")), LevelFilter::DEBUG);
        assert_eq!(level(Some(" WARN\n")), LevelFilter::WARN);
        assert_eq!(level(Some("off")), LevelFilter::OFF);
    }

    #[test]
    fn level_defaults_to_info() {
        for text in [None, Some(""), Some("  "), Some("loud")] {
            assert_eq!(level(text), LevelFilter::INFO, "{text:?}");
        }
    }

    #[test]
    fn init_twice_does_not_panic() {
        init();
        init();
    }
}
