//! The one place that talks HTTP to Alpaca: authentication headers, error mapping and the
//! retry rule. Reads are retried; anything that changes state is sent exactly once, because a
//! resent order could execute twice.

use std::time::Duration;

use reqwest::{
    Method, Response,
    header::{HeaderMap, HeaderValue},
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::{AdapterError, Credentials};

const KEY_HEADER: &str = "APCA-API-KEY-ID";
const SECRET_HEADER: &str = "APCA-API-SECRET-KEY";
/// Attempts for an idempotent read, including the first.
const READ_ATTEMPTS: u32 = 3;
const RETRY_BASE_DELAY: Duration = Duration::from_millis(200);

/// Alpaca's JSON error body, e.g. `{"code": 42210000, "message": "invalid limit_price ..."}`.
#[derive(Deserialize)]
struct ErrorBody {
    code: Option<i64>,
    message: Option<String>,
}

/// An authenticated HTTP client for the Alpaca REST APIs.
#[derive(Debug, Clone)]
pub struct AlpacaHttp {
    client: reqwest::Client,
}

impl AlpacaHttp {
    /// Builds a client that sends the credentials on every request and gives up on a request
    /// after `timeout`.
    pub fn new(credentials: &Credentials, timeout: Duration) -> Result<Self, AdapterError> {
        let header = |value: &str| {
            let mut value = HeaderValue::from_str(value)
                .map_err(|_| AdapterError::Decode("credential is not a valid header".to_owned()))?;
            value.set_sensitive(true);
            Ok::<_, AdapterError>(value)
        };
        let mut headers = HeaderMap::new();
        headers.insert(KEY_HEADER, header(credentials.key_id())?);
        headers.insert(SECRET_HEADER, header(credentials.secret_key())?);
        let client = reqwest::Client::builder()
            .default_headers(headers)
            .timeout(timeout)
            .build()?;
        Ok(Self { client })
    }

    /// `GET url?query`, decoded as JSON. Retried up to [`READ_ATTEMPTS`] times with doubling
    /// delay on transport errors, 408, 429 and 5xx.
    pub(crate) async fn get<T: DeserializeOwned>(
        &self,
        url: &str,
        query: &[(&str, &str)],
    ) -> Result<T, AdapterError> {
        let mut delay = RETRY_BASE_DELAY;
        let mut attempt = 1;
        loop {
            let result = async {
                let response = self.client.get(url).query(query).send().await?;
                Ok::<T, AdapterError>(check(response).await?.json().await?)
            }
            .await;
            match result {
                Err(e) if e.is_retryable() && attempt < READ_ATTEMPTS => {
                    tokio::time::sleep(delay).await;
                    delay *= 2;
                    attempt += 1;
                }
                other => return other,
            }
        }
    }

    /// Sends a state-changing request with an optional JSON body, once, and decodes the JSON
    /// response. Never retried.
    pub(crate) async fn send<B: Serialize + Sync, T: DeserializeOwned>(
        &self,
        method: Method,
        url: &str,
        body: Option<&B>,
    ) -> Result<T, AdapterError> {
        Ok(self.send_raw(method, url, body).await?.json().await?)
    }

    /// Like [`Self::send`] for endpoints whose response body is ignored (e.g. cancel, which
    /// answers 204, or cancel-all, which answers 207 with a per-order list).
    pub(crate) async fn send_discard<B: Serialize + Sync>(
        &self,
        method: Method,
        url: &str,
        body: Option<&B>,
    ) -> Result<(), AdapterError> {
        self.send_raw(method, url, body).await.map(drop)
    }

    async fn send_raw<B: Serialize + Sync>(
        &self,
        method: Method,
        url: &str,
        body: Option<&B>,
    ) -> Result<Response, AdapterError> {
        let mut request = self.client.request(method, url);
        if let Some(body) = body {
            request = request.json(body);
        }
        check(request.send().await?).await
    }
}

/// Turns a non-success response into [`AdapterError::Status`].
async fn check(response: Response) -> Result<Response, AdapterError> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    let text = response.text().await.unwrap_or_default();
    let (code, message) = match serde_json::from_str::<ErrorBody>(&text) {
        Ok(body) => (body.code, body.message.unwrap_or(text)),
        Err(_) => (None, text),
    };
    Err(AdapterError::Status {
        status: status.as_u16(),
        code,
        message,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{header, method, path, query_param},
    };

    use super::*;

    fn http() -> AlpacaHttp {
        let creds = Credentials::new("test-key-id", "test-secret");
        AlpacaHttp::new(&creds, Duration::from_secs(5)).unwrap()
    }

    #[tokio::test]
    async fn get_sends_credentials_and_query() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v2/thing"))
            .and(query_param("a", "1"))
            .and(header(KEY_HEADER, "test-key-id"))
            .and(header(SECRET_HEADER, "test-secret"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
            .expect(1)
            .mount(&server)
            .await;
        let url = format!("{}/v2/thing", server.uri());
        let got: Value = http().get(&url, &[("a", "1")]).await.unwrap();
        assert_eq!(got, json!({"ok": true}));
    }

    #[tokio::test]
    async fn get_retries_server_errors_then_succeeds() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(503))
            .up_to_n_times(2)
            .expect(2)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!(7)))
            .expect(1)
            .mount(&server)
            .await;
        let got: Value = http().get(&server.uri(), &[]).await.unwrap();
        assert_eq!(got, json!(7));
    }

    #[tokio::test]
    async fn get_gives_up_after_three_attempts_and_does_not_retry_client_errors() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/limited"))
            .respond_with(ResponseTemplate::new(429))
            .expect(3)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/missing"))
            .respond_with(ResponseTemplate::new(404))
            .expect(1)
            .mount(&server)
            .await;
        let limited = http()
            .get::<Value>(&format!("{}/limited", server.uri()), &[])
            .await
            .unwrap_err();
        assert_eq!(limited.status(), Some(429));
        assert!(!limited.is_definite_rejection());
        let missing = http()
            .get::<Value>(&format!("{}/missing", server.uri()), &[])
            .await
            .unwrap_err();
        assert_eq!(missing.status(), Some(404));
    }

    #[tokio::test]
    async fn send_is_never_retried_and_maps_the_error_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v2/orders"))
            .respond_with(ResponseTemplate::new(500))
            .expect(1)
            .mount(&server)
            .await;
        let url = format!("{}/v2/orders", server.uri());
        let err = http()
            .send::<_, Value>(Method::POST, &url, Some(&json!({"symbol": "SPY"})))
            .await
            .unwrap_err();
        assert_eq!(err.status(), Some(500));
        assert!(
            !err.is_definite_rejection(),
            "a 5xx leaves the outcome unknown"
        );

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(422).set_body_json(
                    json!({"code": 42210000, "message": "invalid limit_price 290.123"}),
                ),
            )
            .mount(&server)
            .await;
        let err = http()
            .send::<_, Value>(Method::POST, &server.uri(), Some(&json!({})))
            .await
            .unwrap_err();
        assert!(err.is_definite_rejection());
        assert!(matches!(
            err,
            AdapterError::Status { status: 422, code: Some(42_210_000), ref message }
                if message == "invalid limit_price 290.123"
        ));
    }

    #[tokio::test]
    async fn transport_failure_is_unknown_outcome_and_hides_credentials() {
        // Nothing listens on this port.
        let err = http()
            .send::<Value, Value>(Method::POST, "http://127.0.0.1:9/v2/orders", None)
            .await
            .unwrap_err();
        assert!(matches!(err, AdapterError::Transport(_)));
        assert!(!err.is_definite_rejection());
        let shown = format!("{err:?} {err}");
        assert!(!shown.contains("test-secret") && !shown.contains("test-key-id"));
    }

    #[tokio::test]
    async fn send_discard_accepts_an_empty_response() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        http()
            .send_discard::<Value>(Method::DELETE, &server.uri(), None)
            .await
            .unwrap();
    }
}
