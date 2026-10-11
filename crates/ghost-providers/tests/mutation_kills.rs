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

use ghost_providers::{OpenAiCompatibleProvider, ProviderSpec, PROVIDER_SPECS};

fn test_spec() -> ProviderSpec {
    ProviderSpec {
        name: "groq",
        base_url: "https://api.groq.com/openai/v1/chat/completions",
        key_env: "GROQ_API_KEY",
        default_model: "llama-3.3-70b-versatile",
        model_env: "GROQ_MODEL",
    }
}

#[test]
fn openai_compatible_name_exact() {
    let p = OpenAiCompatibleProvider::with_credentials(
        test_spec(),
        String::new(),
        "llama-3.3-70b-versatile".to_string(),
        None,
    );
    // Kill "" and "xyzzy" mutants on name()
    assert_eq!(p.name(), "groq");
}

#[test]
fn openai_compatible_is_available_empty_key() {
    let p = OpenAiCompatibleProvider::with_credentials(
        test_spec(),
        String::new(),
        "llama-3.3-70b-versatile".to_string(),
        None,
    );
    // Kill `true` mutant: must be false when key is empty
    assert!(!p.is_available());
}

#[test]
fn openai_compatible_is_available_with_key() {
    let p = OpenAiCompatibleProvider::with_credentials(
        test_spec(),
        "gsk-test-key".to_string(),
        "llama-3.3-70b-versatile".to_string(),
        None,
    );
    assert!(p.is_available());
}

#[test]
fn provider_specs_all_have_nonempty_names() {
    // Guard against "" mutants across the whole spec table
    for spec in PROVIDER_SPECS {
        assert!(!spec.name.is_empty(), "spec name must not be empty");
        assert_ne!(spec.name, "xyzzy");
    }
    assert!(!PROVIDER_SPECS.is_empty());
}
