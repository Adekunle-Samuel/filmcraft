//! Blocking HTTPS transport (feature `http`): ureq 3 with pure-Rust TLS (rustls + RustCrypto) and
//! the operating system's certificate verifier, and the real providers built on it.
//!
//! Requests are POSTed and the SSE body is streamed through the provider's decoder, checking
//! `cancel` between reads. Rate limits, overload and server errors are retried at most
//! [`MAX_RETRIES`](crate::transport::MAX_RETRIES) times (honouring `retry-after`) before any of the
//! body is read; an error event in the middle of a stream is returned, not retried, so streamed
//! text is never shown twice.

use crate::anthropic::{self, StreamDecoder};
use crate::error::classify_status;
use crate::transport::{ApiKey, check_base_url, join_url, retry_delay};
use crate::types::{ChatRequest, ChatResponse, StreamEvent};
use crate::{Capabilities, LlmError, LlmProvider};
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// The environment variable overriding the Anthropic base URL.
pub const ANTHROPIC_BASE_URL_ENV: &str = "ANTHROPIC_BASE_URL";
/// The environment variable holding the Anthropic API key.
pub const ANTHROPIC_API_KEY_ENV: &str = "ANTHROPIC_API_KEY";

/// Most bytes of an error response body read for its message.
const MAX_ERROR_BODY: u64 = 64 << 10;
/// Read buffer size.
const CHUNK: usize = 16 << 10;

fn agent() -> ureq::Agent {
    let provider = std::sync::Arc::new(rustls_rustcrypto::provider());
    ureq::Agent::config_builder()
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .provider(ureq::tls::TlsProvider::Rustls)
                .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                .unversioned_rustls_crypto_provider(provider)
                .build(),
        )
        .timeout_connect(Some(Duration::from_secs(30)))
        // Time to the response headers; the streamed body itself may take minutes (pings keep
        // it alive).
        .timeout_recv_response(Some(Duration::from_secs(300)))
        // Error statuses are mapped by `classify_status`, which needs the headers and body.
        .http_status_as_error(false)
        .build()
        .into()
}

/// Sleep for `d`, waking every 50 ms to check `cancel`.
fn sleep_cancellable(d: Duration, cancel: &AtomicBool) -> Result<(), LlmError> {
    let start = Instant::now();
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(LlmError::Cancelled);
        }
        let left = d.saturating_sub(start.elapsed());
        if left.is_zero() {
            return Ok(());
        }
        std::thread::sleep(left.min(Duration::from_millis(50)));
    }
}

/// POST `body` and return the successful response (retrying retryable failures).
fn open(agent: &ureq::Agent, url: &str, headers: &[(String, String)], body: &[u8], cancel: &AtomicBool) -> Result<ureq::http::Response<ureq::Body>, LlmError> {
    let mut attempt = 0;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(LlmError::Cancelled);
        }
        match send_once(agent, url, headers, body) {
            Ok(r) => return Ok(r),
            Err(e) => match retry_delay(attempt, &e) {
                Some(d) => {
                    sleep_cancellable(d, cancel)?;
                    attempt += 1;
                }
                None => return Err(e),
            },
        }
    }
}

fn send_once(agent: &ureq::Agent, url: &str, headers: &[(String, String)], body: &[u8]) -> Result<ureq::http::Response<ureq::Body>, LlmError> {
    let mut rb = agent.post(url);
    for (k, v) in headers {
        rb = rb.header(k.as_str(), v.as_str());
    }
    let resp = rb.send(body).map_err(|e| LlmError::Network(e.to_string()))?;
    let status = resp.status().as_u16();
    if (200..300).contains(&status) {
        return Ok(resp);
    }
    let retry_after = resp.headers().get("retry-after").and_then(|v| v.to_str().ok()).map(str::to_string);
    let mut text = String::new();
    // A body that fails to read still has a useful status.
    let _ = resp.into_body().into_reader().take(MAX_ERROR_BODY).read_to_string(&mut text);
    Err(classify_status(status, retry_after.as_deref(), &text))
}

/// Read the body in chunks into `feed` until it reports completion or the body ends.
fn pump(resp: ureq::http::Response<ureq::Body>, cancel: &AtomicBool, feed: &mut dyn FnMut(&[u8]) -> Result<bool, LlmError>) -> Result<(), LlmError> {
    let mut reader = resp.into_body().into_reader();
    let mut buf = vec![0u8; CHUNK];
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(LlmError::Cancelled);
        }
        let n = match reader.read(&mut buf) {
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(LlmError::Network(e.to_string())),
        };
        if n == 0 {
            return Ok(());
        }
        if feed(buf.get(..n).unwrap_or_default())? {
            return Ok(());
        }
    }
}

/// POST a streaming request and feed the body to `feed` (which returns true when the response is
/// complete). Errors are scrubbed of `key`.
pub(crate) fn stream(
    agent: &ureq::Agent,
    url: &str,
    headers: &[(String, String)],
    body: &[u8],
    cancel: &AtomicBool,
    key: Option<&ApiKey>,
    feed: &mut dyn FnMut(&[u8]) -> Result<bool, LlmError>,
) -> Result<(), LlmError> {
    open(agent, url, headers, body, cancel).and_then(|resp| pump(resp, cancel, feed)).map_err(|e| match key {
        Some(k) => k.redact_error(e),
        None => e,
    })
}

/// The Claude Messages API.
pub struct AnthropicProvider {
    api_key: ApiKey,
    base_url: String,
    agent: ureq::Agent,
}

impl core::fmt::Debug for AnthropicProvider {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("AnthropicProvider").field("base_url", &self.base_url).field("api_key", &self.api_key).finish()
    }
}

impl AnthropicProvider {
    /// A provider using `ANTHROPIC_BASE_URL` when set, else `https://api.anthropic.com`.
    pub fn new(api_key: ApiKey) -> Result<Self, LlmError> {
        let base = std::env::var(ANTHROPIC_BASE_URL_ENV).ok().filter(|s| !s.trim().is_empty());
        Self::with_base_url(api_key, base.as_deref().unwrap_or(anthropic::DEFAULT_BASE_URL))
    }

    /// A provider for an explicit base URL (checked by [`check_base_url`]).
    pub fn with_base_url(api_key: ApiKey, base_url: &str) -> Result<Self, LlmError> {
        let base_url = base_url.trim();
        check_base_url(base_url)?;
        Ok(Self { api_key, base_url: base_url.to_string(), agent: agent() })
    }

    /// A provider keyed from `ANTHROPIC_API_KEY`.
    pub fn from_env() -> Result<Self, LlmError> {
        let key = std::env::var(ANTHROPIC_API_KEY_ENV)
            .map_err(|_| LlmError::Unsupported(format!("no Anthropic API key: set {ANTHROPIC_API_KEY_ENV} or add a key in Assistant settings")))?;
        Self::new(ApiKey::new(key)?)
    }

    /// The base URL in use.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }
}

impl LlmProvider for AnthropicProvider {
    fn name(&self) -> &str {
        "anthropic"
    }

    fn send(&self, req: &ChatRequest, on_event: &mut dyn FnMut(StreamEvent), cancel: &AtomicBool) -> Result<ChatResponse, LlmError> {
        let body = anthropic::encode_request_bytes(req)?;
        let mut headers: Vec<(String, String)> = anthropic::headers(req).into_iter().map(|(k, v)| (k.to_string(), v)).collect();
        headers.push(("x-api-key".into(), self.api_key.expose().to_string()));
        headers.push(("content-type".into(), "application/json".into()));
        headers.push(("accept".into(), "text/event-stream".into()));
        let url = join_url(&self.base_url, anthropic::MESSAGES_PATH);
        let mut decoder = StreamDecoder::new();
        stream(&self.agent, &url, &headers, &body, cancel, Some(&self.api_key), &mut |chunk| {
            decoder.push(chunk, on_event)?;
            Ok(decoder.is_complete())
        })?;
        decoder.finish(on_event).map_err(|e| self.api_key.redact_error(e))
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::all()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_debug_hides_the_key() {
        let p = AnthropicProvider::with_base_url(ApiKey::new("sk-ant-very-secret").unwrap(), "https://api.anthropic.com").unwrap();
        let d = format!("{p:?}");
        assert!(d.contains("api.anthropic.com") && !d.contains("very-secret"), "{d}");
        assert!(AnthropicProvider::with_base_url(ApiKey::new("k").unwrap(), "http://api.example.com").is_err());
        assert!(AnthropicProvider::with_base_url(ApiKey::new("k").unwrap(), "http://localhost:8080").is_ok());
    }

    #[test]
    fn cancelled_sleep_returns_promptly() {
        let cancel = AtomicBool::new(true);
        let t = Instant::now();
        assert_eq!(sleep_cancellable(Duration::from_secs(30), &cancel), Err(LlmError::Cancelled));
        assert!(t.elapsed() < Duration::from_secs(1));
        assert_eq!(sleep_cancellable(Duration::from_millis(1), &AtomicBool::new(false)), Ok(()));
    }

    /// Network smoke test of the TLS setup and status mapping (opt-in: `--ignored`).
    #[test]
    #[ignore = "needs network"]
    fn live_bad_key_maps_to_auth() {
        let p = AnthropicProvider::with_base_url(ApiKey::new("sk-ant-not-a-real-key").unwrap(), anthropic::DEFAULT_BASE_URL).unwrap();
        let req = ChatRequest { messages: vec![crate::Message::user_text("hi")], max_tokens: 16, ..ChatRequest::default() };
        assert_eq!(p.send(&req, &mut |_| {}, &AtomicBool::new(false)), Err(LlmError::Auth));
    }

    #[test]
    fn cancelled_before_sending_never_touches_the_network() {
        // A closed loopback port: if the request were made it would fail with a network error.
        let p = AnthropicProvider::with_base_url(ApiKey::new("k").unwrap(), "http://127.0.0.1:9").unwrap();
        let cancel = AtomicBool::new(true);
        assert_eq!(p.send(&ChatRequest::default(), &mut |_| {}, &cancel), Err(LlmError::Cancelled));
    }
}
