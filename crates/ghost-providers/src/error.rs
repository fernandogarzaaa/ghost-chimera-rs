//! Provider error types.
//!
//! Mirrors the failure modes of Python's `BaseProvider.chat()`.

use thiserror::Error;

/// Every way a model provider call can fail.
#[derive(Debug, Error)]
pub enum ProviderError {
    /// Credentials missing or rejected (HTTP 401/403).
    #[error("provider auth failure: {0}")]
    Auth(String),

    /// Rate limited (HTTP 429). `retry_after_s` may be absent.
    #[error("provider rate limited{}", match .0 { Some(s) => format!(" (retry after {s}s)"), None => String::new() })]
    RateLimit(Option<u64>),

    /// Network / transport failure.
    #[error("provider network error: {0}")]
    Network(String),

    /// The provider returned something we cannot parse.
    #[error("provider invalid response: {0}")]
    InvalidResponse(String),

    /// The provider is not configured (no credentials).
    #[error("provider unavailable: {0}")]
    Unavailable(String),

    /// The call timed out.
    #[error("provider timeout: {0}")]
    Timeout(String),

    /// The provider's spend budget is exhausted; the request was NOT sent.
    #[error("provider budget exceeded: {0}")]
    BudgetExceeded(String),

    /// The spend could not be journaled (the reply is withheld).
    #[error("provider ledger error: {0}")]
    Ledger(String),
}

impl From<reqwest::Error> for ProviderError {
    fn from(err: reqwest::Error) -> Self {
        if err.is_timeout() {
            Self::Timeout(err.to_string())
        } else if err.is_connect() {
            Self::Network(err.to_string())
        } else if err.is_decode() {
            Self::InvalidResponse(err.to_string())
        } else if let Some(status) = err.status() {
            match status.as_u16() {
                401 | 403 => Self::Auth(err.to_string()),
                429 => Self::RateLimit(None),
                _ => Self::Network(err.to_string()),
            }
        } else {
            Self::Network(err.to_string())
        }
    }
}
