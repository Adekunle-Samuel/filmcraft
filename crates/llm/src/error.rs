//! [`LlmError`]: every way a model call can fail, with messages a user can act on.

use core::time::Duration;

/// A failed model call. Messages never contain the API key.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum LlmError {
    #[error("the provider rejected the API key: check the key in Assistant settings (or the ANTHROPIC_API_KEY / OPENAI_API_KEY environment variable)")]
    Auth,
    #[error("the provider is rate limiting requests: {}", retry_hint(*.retry_after))]
    RateLimited { retry_after: Option<Duration> },
    #[error("the provider is overloaded or had a server error: try again in a moment")]
    Overloaded,
    #[error("the provider rejected the request: {0}")]
    BadRequest(String),
    #[error("the request or response is too large: start a new conversation, or attach fewer or smaller images")]
    TooLarge,
    #[error("network error: {0} (check your connection and the provider URL in Assistant settings)")]
    Network(String),
    #[error("cancelled")]
    Cancelled,
    #[error("could not read the provider's response: {0}")]
    Decode(String),
    #[error("{0}")]
    Unsupported(String),
}

impl LlmError {
    /// Worth retrying the same request after a pause (rate limits, overload, server errors).
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::RateLimited { .. } | Self::Overloaded)
    }
}

fn retry_hint(after: Option<Duration>) -> String {
    match after {
        Some(d) => format!("try again in {} s", d.as_secs().max(1)),
        None => "wait a minute and try again".into(),
    }
}

/// Map an HTTP error status (and the response body and `retry-after` header) to an error.
/// Pure: unit-tested without network.
pub fn classify_status(status: u16, retry_after: Option<&str>, body: &str) -> LlmError {
    match status {
        401 | 403 => LlmError::Auth,
        429 => LlmError::RateLimited { retry_after: retry_after.and_then(parse_retry_after) },
        413 => LlmError::TooLarge,
        400 | 404 | 405 | 409 | 410 | 415 | 422 => LlmError::BadRequest(api_error_message(body).unwrap_or_else(|| format!("HTTP {status}"))),
        500..=599 => LlmError::Overloaded,
        _ => LlmError::BadRequest(api_error_message(body).map(|m| format!("HTTP {status}: {m}")).unwrap_or_else(|| format!("HTTP {status}"))),
    }
}

/// The `retry-after` header as a delay: whole or fractional seconds (HTTP dates are ignored),
/// capped at ten minutes.
pub fn parse_retry_after(v: &str) -> Option<Duration> {
    let s: f64 = v.trim().parse().ok()?;
    if !s.is_finite() || s < 0.0 {
        return None;
    }
    Some(Duration::from_millis((s.min(600.0) * 1000.0) as u64))
}

/// Longest error message kept from a response body.
const MAX_ERROR_MESSAGE: usize = 500;

/// `error.message` from an Anthropic or OpenAI error body (truncated), or a short plain body.
pub fn api_error_message(body: &str) -> Option<String> {
    let msg = match serde_json::from_str::<serde_json::Value>(body) {
        Ok(v) => v.get("error").and_then(|e| e.get("message").and_then(|m| m.as_str()).or_else(|| e.as_str())).map(str::to_string),
        Err(_) => {
            let t = body.trim();
            (!t.is_empty() && !t.starts_with('<')).then(|| t.to_string())
        }
    }?;
    Some(truncate_chars(&msg, MAX_ERROR_MESSAGE))
}

/// Map an Anthropic SSE `error` event (`{"type":"error","error":{"type":..,"message":..}}`).
pub fn classify_stream_error(v: &serde_json::Value) -> LlmError {
    let err = v.get("error");
    let kind = err.and_then(|e| e.get("type")).and_then(|t| t.as_str()).unwrap_or_default();
    let msg = err.and_then(|e| e.get("message")).and_then(|m| m.as_str()).map(|m| truncate_chars(m, MAX_ERROR_MESSAGE));
    match kind {
        "authentication_error" | "permission_error" => LlmError::Auth,
        "rate_limit_error" => LlmError::RateLimited { retry_after: None },
        "overloaded_error" | "api_error" | "timeout_error" => LlmError::Overloaded,
        "request_too_large" => LlmError::TooLarge,
        _ => LlmError::BadRequest(msg.unwrap_or_else(|| if kind.is_empty() { "unknown error".into() } else { kind.to_string() })),
    }
}

/// At most `n` characters of `s` (never splits a character).
pub(crate) fn truncate_chars(s: &str, n: usize) -> String {
    match s.char_indices().nth(n) {
        Some((i, _)) => format!("{}…", s.get(..i).unwrap_or_default()),
        None => s.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses_map_to_actionable_errors() {
        assert_eq!(classify_status(401, None, ""), LlmError::Auth);
        assert_eq!(classify_status(403, None, ""), LlmError::Auth);
        assert_eq!(classify_status(429, Some("7"), ""), LlmError::RateLimited { retry_after: Some(Duration::from_secs(7)) });
        assert_eq!(classify_status(429, Some("Wed, 21 Oct 2015 07:28:00 GMT"), ""), LlmError::RateLimited { retry_after: None });
        assert_eq!(classify_status(529, None, ""), LlmError::Overloaded);
        assert_eq!(classify_status(503, None, "<html>"), LlmError::Overloaded);
        assert_eq!(classify_status(413, None, ""), LlmError::TooLarge);
        let body = r#"{"type":"error","error":{"type":"invalid_request_error","message":"max_tokens: too big"}}"#;
        assert_eq!(classify_status(400, None, body), LlmError::BadRequest("max_tokens: too big".into()));
        assert_eq!(classify_status(400, None, r#"{"error":{"message":"model not found"}}"#), LlmError::BadRequest("model not found".into()));
        assert_eq!(classify_status(400, None, ""), LlmError::BadRequest("HTTP 400".into()));
        assert!(classify_status(429, None, "").is_retryable());
        assert!(classify_status(500, None, "").is_retryable());
        assert!(!classify_status(400, None, "").is_retryable());
    }

    #[test]
    fn retry_after_is_bounded() {
        assert_eq!(parse_retry_after("1.5"), Some(Duration::from_millis(1500)));
        assert_eq!(parse_retry_after("1e9"), Some(Duration::from_secs(600)));
        assert_eq!(parse_retry_after("-1"), None);
        assert_eq!(parse_retry_after("NaN"), None);
        assert_eq!(parse_retry_after("inf"), None);
    }

    #[test]
    fn long_messages_are_truncated_on_char_boundaries() {
        let body = format!(r#"{{"error":{{"message":"{}"}}}}"#, "é".repeat(2000));
        let LlmError::BadRequest(m) = classify_status(400, None, &body) else { panic!() };
        assert_eq!(m.chars().count(), MAX_ERROR_MESSAGE + 1);
        assert!(LlmError::RateLimited { retry_after: None }.to_string().contains("try again"));
    }

    #[test]
    fn stream_errors_map_by_type() {
        let v = serde_json::json!({"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}});
        assert_eq!(classify_stream_error(&v), LlmError::Overloaded);
        assert_eq!(classify_stream_error(&serde_json::json!({})), LlmError::BadRequest("unknown error".into()));
    }
}
