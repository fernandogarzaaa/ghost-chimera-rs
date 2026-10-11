//! Gemini adapter: extensions directory hooks.

use serde_json::json;

use super::{which, HostAdapter, InstallPayload};

/// Gemini via its extensions directory.
pub struct GeminiAdapter;

impl HostAdapter for GeminiAdapter {
    fn id(&self) -> &'static str {
        "gemini"
    }

    fn name(&self) -> &'static str {
        "Gemini"
    }

    fn detect(&self) -> bool {
        which("gemini")
    }

    fn install(&self) -> InstallPayload {
        InstallPayload::installed(self.id())
            .artifact(
                "extension",
                json!({
                    "name": "ghost",
                    "version": "0.1.0",
                    "hooks": {
                        "before_prompt": {"command": "ghost-hook gemini-prompt"},
                        "after_tool": {"command": "ghost-hook gemini-tool"},
                    },
                    "commands": ["/ghost"],
                }),
            )
            .note("Drop into the Gemini extensions dir; hooks POST hook JSON to ghost-hook.")
    }
}
