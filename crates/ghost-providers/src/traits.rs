//! The `ModelProvider` trait.
//!
//! Mirrors Python's `BaseProvider` (`model_layer/base_provider.py`).

use crate::error::ProviderError;

/// A chat-completions model provider.
///
/// Implementations must be `Send + Sync` so a registry can be shared
/// across threads.
pub trait ModelProvider: Send + Sync {
    /// Provider id, e.g. `"nebius"`, `"anthropic"`, `"groq"`.
    fn name(&self) -> &str;

    /// Model id used for requests (journaled in the cost ledger).
    /// Defaults to `"unknown"` for providers that do not expose one.
    fn model(&self) -> &str {
        "unknown"
    }

    /// Send a chat request, return the assistant's text reply.
    fn chat(&self, system: &str, user: &str) -> Result<String, ProviderError>;

    /// Configuration problems; empty when the provider is fully configured.
    fn validate_config(&self) -> Vec<String>;

    /// True when credentials are present (no network check).
    fn is_available(&self) -> bool;
}
