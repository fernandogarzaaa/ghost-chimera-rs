//! Hermes adapter: external memory provider.

use serde_json::json;

use super::{which, HostAdapter, InstallPayload};

/// Hermes via the external memory-provider interface.
pub struct HermesAdapter;

impl HostAdapter for HermesAdapter {
    fn id(&self) -> &'static str {
        "hermes"
    }

    fn name(&self) -> &'static str {
        "Hermes"
    }

    fn detect(&self) -> bool {
        which("hermes")
    }

    fn install(&self) -> InstallPayload {
        InstallPayload::installed(self.id())
            .artifact(
                "memory_provider",
                json!({
                    "name": "ghost",
                    "capabilities": ["prefetch", "submit_feedback"],
                    "endpoint": "local ghost transport /query_context",
                }),
            )
            .note(
                "Register ghost as an external memory provider; Hermes calls prefetch() per turn.",
            )
    }
}
