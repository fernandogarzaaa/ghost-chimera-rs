//! OpenCode adapter: plugin/MCP surface.

use serde_json::json;

use super::{which, HostAdapter, InstallPayload};

/// OpenCode via plugin/MCP surface (event mapping + command passthrough).
pub struct OpenCodeAdapter;

impl HostAdapter for OpenCodeAdapter {
    fn id(&self) -> &'static str {
        "opencode"
    }

    fn name(&self) -> &'static str {
        "OpenCode"
    }

    fn detect(&self) -> bool {
        which("opencode")
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
            .note("Register the ghost plugin with OpenCode; context blocks come from inject_context().")
    }
}
