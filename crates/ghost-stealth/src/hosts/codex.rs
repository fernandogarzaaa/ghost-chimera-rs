//! Codex adapter: plugin/MCP surface.

use serde_json::json;

use super::{which, HostAdapter, InstallPayload};

/// Codex via plugin/MCP surface.
pub struct CodexAdapter;

impl HostAdapter for CodexAdapter {
    fn id(&self) -> &'static str {
        "codex"
    }

    fn name(&self) -> &'static str {
        "Codex"
    }

    fn detect(&self) -> bool {
        which("codex")
    }

    fn install(&self) -> InstallPayload {
        InstallPayload::installed(self.id())
            .artifact(
                "plugin",
                json!({
                    "name": "ghost",
                    "skills": ["ghost-context"],
                    "mcp": {"ghost": {"command": "ghost-hook", "args": ["mcp"]}},
                    "slash": {"/ghost": "Ghost status, memory, and workflow commands"},
                }),
            )
            .note(
                "Register the ghost plugin with Codex; context blocks come from inject_context().",
            )
    }
}
