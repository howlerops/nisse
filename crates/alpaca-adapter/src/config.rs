//! Adapter configuration and credentials.
//!
//! Credentials are read from the environment of the engine host and nowhere else. They are
//! never written to the repo, to logs or to error messages, and tests use placeholder values
//! against a local mock server.

use std::fmt;

use crate::AdapterError;

/// Environment variable holding the Alpaca API key id.
pub const ENV_KEY_ID: &str = "APCA_API_KEY_ID";
/// Environment variable holding the Alpaca API secret key.
pub const ENV_SECRET_KEY: &str = "APCA_API_SECRET_KEY";

/// Alpaca API credentials. `Debug` never prints the values.
#[derive(Clone)]
pub struct Credentials {
    key_id: String,
    secret_key: String,
}

impl Credentials {
    #[must_use]
    pub fn new(key_id: impl Into<String>, secret_key: impl Into<String>) -> Self {
        Self {
            key_id: key_id.into(),
            secret_key: secret_key.into(),
        }
    }

    /// Reads [`ENV_KEY_ID`] and [`ENV_SECRET_KEY`].
    pub fn from_env() -> Result<Self, AdapterError> {
        let read = |name: &'static str| match std::env::var(name) {
            Ok(value) if !value.is_empty() => Ok(value),
            _ => Err(AdapterError::MissingEnv(name)),
        };
        Ok(Self::new(read(ENV_KEY_ID)?, read(ENV_SECRET_KEY)?))
    }

    pub(crate) fn key_id(&self) -> &str {
        &self.key_id
    }

    pub(crate) fn secret_key(&self) -> &str {
        &self.secret_key
    }
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Credentials(<redacted>)")
    }
}

/// Which market data feed to read. IEX is free; SIP is the paid consolidated feed. The choice
/// is an open question in PLAN.md, so it is configuration, not code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Feed {
    Iex,
    Sip,
}

impl Feed {
    /// The value of Alpaca's `feed` query parameter.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Iex => "iex",
            Self::Sip => "sip",
        }
    }
}

/// Endpoints and options. Base URLs have no trailing slash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlpacaConfig {
    /// Trading REST API, e.g. `https://paper-api.alpaca.markets`.
    pub trading_base_url: String,
    /// Market data REST API, e.g. `https://data.alpaca.markets`.
    pub data_base_url: String,
    /// Order update websocket, e.g. `wss://paper-api.alpaca.markets/stream`.
    pub stream_url: String,
    pub feed: Feed,
}

impl AlpacaConfig {
    /// The paper trading environment. There is deliberately no `live()` constructor yet: the
    /// live endpoints are added in Phase 9.
    #[must_use]
    pub fn paper(feed: Feed) -> Self {
        Self {
            trading_base_url: "https://paper-api.alpaca.markets".to_owned(),
            data_base_url: "https://data.alpaca.markets".to_owned(),
            stream_url: "wss://paper-api.alpaca.markets/stream".to_owned(),
            feed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_never_prints_credential_values() {
        let creds = Credentials::new("PKTESTKEYID", "placeholder-secret-value");
        let shown = format!("{creds:?}");
        assert!(!shown.contains("PKTESTKEYID"));
        assert!(!shown.contains("placeholder-secret-value"));
    }

    #[test]
    fn paper_config_points_at_paper_endpoints() {
        let config = AlpacaConfig::paper(Feed::Iex);
        assert!(config.trading_base_url.contains("paper-api"));
        assert!(config.stream_url.starts_with("wss://paper-api"));
        assert_eq!(config.feed.as_str(), "iex");
        assert_eq!(Feed::Sip.as_str(), "sip");
    }
}
