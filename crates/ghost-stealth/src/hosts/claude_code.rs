//! Claude Code adapter: settings.json hooks.

use serde_json::json;

use super::{which, HostAdapter, InstallPayload};

/// Claude Code via `~/.claude/settings.json` hooks.
pub struct ClaudeCodeAdapter;

impl HostAdapter for ClaudeCodeAdapter {
    fn id(&self) -> &'static str {
        "claude-code"
    }

    fn name(&self) -> &'static str {
        "Claude Code"
    }

    fn detect(&self) -> bool {
        which("claude")
    }

    fn install(&self) -> InstallPayload {
        InstallPayload::installed(self.id())
            .artifact(
                "settings_snippet",
                json!({
                    "hooks": {
                        "UserPromptSubmit": [
                            {"matcher": "", "hooks": [{"type": "command", "command": "ghost-hook prompt"}]}
                        ],
                        "SessionStart": [
                            {"matcher": "", "hooks": [{"type": "command", "command": "ghost-hook start"}]}
                        ],
                        "SessionEnd": [
                            {"matcher": "", "hooks": [{"type": "command", "command": "ghost-hook end"}]}
                        ],
                    }
                }),
            )
            .note("Merge into ~/.claude/settings.json; ghost-hook emits Ghost events per hook.")
    }
}
