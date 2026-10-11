//! Model provider integrations for GHOST-Chimera.
//!
//! Mirrors `ghostchimera/model_layer/`: a `ModelProvider` trait, one
//! `reqwest`-based OpenAI-compatible client covering 24 providers,
//! a native Anthropic client, a registry built from the environment,
//! and a durable cost ledger.

pub mod anthropic;
pub mod error;
pub mod ledger;
pub mod openai_compatible;
pub mod registry;
pub mod traits;

pub use anthropic::AnthropicProvider;
pub use error::ProviderError;
pub use ledger::CostLedger;
pub use openai_compatible::{OpenAiCompatibleProvider, ProviderSpec, PROVIDER_SPECS};
pub use registry::ProviderRegistry;
pub use traits::ModelProvider;
