//! Transport policy shared by the HTTP providers, as pure functions (unit-tested without
//! network, and compiled without the `http` feature so settings UIs can validate input):
//! [`ApiKey`] (never printed), the provider URL policy and the retry schedule.

use crate::LlmError;
use core::time::Duration;

/// An API key. Its `Debug` output is redacted and it has no `Display` or serde impls, so it can't
/// leak into logs, error strings, project files or UI state by accident.
#[derive(Clone, PartialEq, Eq)]
pub struct ApiKey(String);

impl ApiKey {
    /// A key from user input (trimmed). Empty keys and keys with control characters (which would
    /// corrupt a header) are refused.
    pub fn new(key: impl Into<String>) -> Result<Self, LlmError> {
        let key = key.into().trim().to_string();
        if key.is_empty() || key.chars().any(char::is_control) {
            return Err(LlmError::Unsupported("the API key is empty or contains invalid characters".into()));
        }
        Ok(Self(key))
    }

    /// The key itself, only for building the request header.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// `s` with any occurrence of the key replaced by `[redacted]`.
    pub fn redact(&self, s: &str) -> String {
        s.replace(&self.0, "[redacted]")
    }

    /// Redact the key out of an error's message.
    pub fn redact_error(&self, e: LlmError) -> LlmError {
        match e {
            LlmError::BadRequest(m) => LlmError::BadRequest(self.redact(&m)),
            LlmError::Network(m) => LlmError::Network(self.redact(&m)),
            LlmError::Decode(m) => LlmError::Decode(self.redact(&m)),
            LlmError::Unsupported(m) => LlmError::Unsupported(self.redact(&m)),
            other => other,
        }
    }
}

impl core::fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("ApiKey([redacted])")
    }
}

/// Check a provider base URL: `https://` anywhere, plain `http://` only to this machine
/// (`localhost`, `127.0.0.1`, `[::1]`), no user name or password in the URL.
pub fn check_base_url(url: &str) -> Result<(), LlmError> {
    let bad = |why: &str| Err(LlmError::Unsupported(format!("invalid provider URL: {why}")));
    if url.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return bad("it contains spaces or control characters");
    }
    let lower = url.to_ascii_lowercase();
    let (secure, rest) = if let Some(r) = lower.strip_prefix("https://") {
        (true, r)
    } else if let Some(r) = lower.strip_prefix("http://") {
        (false, r)
    } else {
        return bad("it must start with https:// (or http:// for a server on this computer)");
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.contains('@') {
        return bad("it must not contain a user name or password");
    }
    let host = if let Some(v6) = authority.strip_prefix('[') {
        match v6.split_once(']') {
            Some((h, _)) => h,
            None => return bad("unterminated IPv6 address"),
        }
    } else {
        authority.split(':').next().unwrap_or_default()
    };
    if host.is_empty() {
        return bad("it has no host");
    }
    if !secure && !is_loopback(host) {
        return bad("plain http:// is only allowed for localhost; use https://");
    }
    Ok(())
}

fn is_loopback(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "::1")
}

/// `base` (without trailing slashes) followed by `path`.
pub fn join_url(base: &str, path: &str) -> String {
    format!("{}{}", base.trim_end_matches('/'), path)
}

/// Retries after the first attempt, for rate limits, overload and server errors.
pub const MAX_RETRIES: u32 = 3;
/// A `retry-after` longer than this is not waited out: the error goes to the user instead.
pub const MAX_RETRY_WAIT: Duration = Duration::from_secs(60);

/// How long to wait before retry number `attempt + 1` after `err`, or `None` to give up:
/// the server's `retry-after` when given, else 1 s, 2 s, 4 s.
pub fn retry_delay(attempt: u32, err: &LlmError) -> Option<Duration> {
    if attempt >= MAX_RETRIES || !err.is_retryable() {
        return None;
    }
    let backoff = Duration::from_secs(1u64 << attempt.min(6));
    match err {
        LlmError::RateLimited { retry_after: Some(d) } if *d > MAX_RETRY_WAIT => None,
        LlmError::RateLimited { retry_after: Some(d) } => Some(*d),
        _ => Some(backoff),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_policy() {
        for ok in [
            "https://api.anthropic.com",
            "https://example.com:8443/v1/",
            "http://localhost:11434/v1",
            "http://127.0.0.1:1234",
            "http://[::1]:8080/v1",
            "HTTP://LOCALHOST",
        ] {
            assert!(check_base_url(ok).is_ok(), "{ok}");
        }
        for bad in [
            "http://example.com/v1",
            "http://localhost@evil.example/v1",
            "https://user:pw@example.com",
            "http://localhost.evil.example",
            "http://127.0.0.2",
            "ftp://localhost",
            "localhost:11434",
            "https://",
            "http://[::1",
            "https://exa mple.com",
            "https://example.com\r\nX: y",
        ] {
            assert!(check_base_url(bad).is_err(), "{bad}");
        }
        assert_eq!(join_url("http://localhost:11434/v1/", "/chat/completions"), "http://localhost:11434/v1/chat/completions");
    }

    #[test]
    fn keys_never_print() {
        let k = ApiKey::new("  sk-ant-secret  ").unwrap();
        assert_eq!(k.expose(), "sk-ant-secret");
        assert!(!format!("{k:?}").contains("secret"));
        assert!(ApiKey::new("").is_err());
        assert!(ApiKey::new("a\nb").is_err());
        let e = k.redact_error(LlmError::BadRequest("bad key sk-ant-secret".into()));
        assert!(!e.to_string().contains("secret"));
    }

    #[test]
    fn retry_schedule() {
        let over = LlmError::Overloaded;
        assert_eq!(retry_delay(0, &over), Some(Duration::from_secs(1)));
        assert_eq!(retry_delay(2, &over), Some(Duration::from_secs(4)));
        assert_eq!(retry_delay(3, &over), None);
        assert_eq!(retry_delay(0, &LlmError::RateLimited { retry_after: Some(Duration::from_secs(9)) }), Some(Duration::from_secs(9)));
        assert_eq!(retry_delay(0, &LlmError::RateLimited { retry_after: Some(Duration::from_secs(90)) }), None);
        assert_eq!(retry_delay(1, &LlmError::RateLimited { retry_after: None }), Some(Duration::from_secs(2)));
        assert_eq!(retry_delay(0, &LlmError::Auth), None);
        assert_eq!(retry_delay(0, &LlmError::BadRequest("x".into())), None);
        assert_eq!(retry_delay(u32::MAX, &over), None);
    }
}
