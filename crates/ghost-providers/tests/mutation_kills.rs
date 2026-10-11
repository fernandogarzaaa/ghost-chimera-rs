//! Mutation-killing tests for ghost-providers.
//!
//! Kills mutants that replace:
//! - name() -> &str with "" or "xyzzy"
//! - is_available() -> bool with true

use ghost_providers::{AnthropicProvider, ModelProvider};

#[test]
fn anthropic_name_exact() {
    let p = AnthropicProvider::with_credentials(String::new(), "test-model".to_string(), None);
    // Kill "" and "xyzzy" mutants
    assert_eq!(p.name(), "anthropic");
}

#[test]
fn anthropic_is_available_empty_key() {
    let p = AnthropicProvider::with_credentials(String::new(), "test-model".to_string(), None);
    // Kill `true` mutant: must be false when key is empty
    assert!(!p.is_available());
}

#[test]
fn anthropic_is_available_with_key() {
    let p = AnthropicProvider::with_credentials("sk-ant-key".to_string(), "test-model".to_string(), None);
    assert!(p.is_available());
}
