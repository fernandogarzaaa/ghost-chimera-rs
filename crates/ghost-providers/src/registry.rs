//! Provider registry: name -> constructed provider.
//!
//! Mirrors the provider wiring in `model_layer/__init__.py`.

use std::sync::Arc;

use crate::anthropic::AnthropicProvider;
use crate::openai_compatible::{OpenAiCompatibleProvider, PROVIDER_SPECS};
use crate::traits::ModelProvider;

/// All providers whose credentials are present in the environment.
pub struct ProviderRegistry {
    providers: Vec<Arc<dyn ModelProvider>>,
}

impl ProviderRegistry {
    /// Construct every available provider from the environment.
    ///
    /// Reads `<KEY_ENV>` for each of the 24 OpenAI-compatible providers and
    /// `ANTHROPIC_API_KEY` for Anthropic. Providers without credentials are
    /// skipped (never constructed half-configured).
    pub fn from_env() -> Self {
        let mut providers: Vec<Arc<dyn ModelProvider>> = Vec::new();
        for spec in PROVIDER_SPECS {
            let p = OpenAiCompatibleProvider::from_spec(*spec);
            if p.is_available() {
                providers.push(Arc::new(p));
            }
        }
        let anthropic = AnthropicProvider::from_env();
        if anthropic.is_available() {
            providers.push(Arc::new(anthropic));
        }
        Self { providers }
    }

    /// Names of all available providers.
    pub fn available_names(&self) -> Vec<&str> {
        self.providers.iter().map(|p| p.name()).collect()
    }

    /// Look up a provider by name.
    pub fn get(&self, name: &str) -> Option<Arc<dyn ModelProvider>> {
        self.providers.iter().find(|p| p.name() == name).cloned()
    }

    /// Number of available providers.
    pub fn len(&self) -> usize {
        self.providers.len()
    }

    /// True when no provider has credentials configured.
    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Serializes the env-mutating tests: Rust runs tests in parallel threads
    /// sharing one process environment.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn empty_env_yields_no_providers() {
        let _guard = ENV_LOCK.lock().expect("env lock");
        // Every known key env var must be absent for this test.
        for spec in PROVIDER_SPECS {
            std::env::remove_var(spec.key_env);
        }
        std::env::remove_var("ANTHROPIC_API_KEY");
        let registry = ProviderRegistry::from_env();
        assert!(registry.is_empty());
        assert!(registry.get("nebius").is_none());
    }

    #[test]
    fn registry_picks_up_configured_providers() {
        let _guard = ENV_LOCK.lock().expect("env lock");
        std::env::set_var("NEBIUS_API_KEY", "test-key");
        std::env::set_var("GROQ_API_KEY", "test-key");
        let registry = ProviderRegistry::from_env();
        let names = registry.available_names();
        assert!(names.contains(&"nebius"), "names: {names:?}");
        assert!(names.contains(&"groq"), "names: {names:?}");
        assert!(registry.get("nebius").is_some());
        assert!(registry.get("openai").is_none());
        std::env::remove_var("NEBIUS_API_KEY");
        std::env::remove_var("GROQ_API_KEY");
    }
}
