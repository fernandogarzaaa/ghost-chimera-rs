//! OpenClaw adapter: context-engine lifecycle (ingest/assemble/after-turn).
//!
//! Port of `OpenClawAdapter` from `hosts.py` including the PR #117 fix:
//! `install()` returns the real lifecycle-hook config, never a stub.

use serde_json::json;

use super::{which, HostAdapter, InstallPayload};

/// OpenClaw via its context-engine lifecycle.
pub struct OpenClawAdapter;

impl HostAdapter for OpenClawAdapter {
    fn id(&self) -> &'static str {
        "openclaw"
    }

    fn name(&self) -> &'static str {
        "OpenClaw"
    }

    fn detect(&self) -> bool {
        which("openclaw")
    }

    fn install(&self) -> InstallPayload {
        InstallPayload::installed(self.id())
            .artifact(
                "context_engine_snippet",
                json!({
                    "lifecycle_hooks": {
                        "ingest": {"command": "ghost-hook openclaw-ingest"},
                        "assemble": {"command": "ghost-hook openclaw-assemble"},
                        "after-turn": {"command": "ghost-hook openclaw-after-turn"},
                    }
                }),
            )
            .artifact(
                "capabilities",
                json!({
                    "observes_session": true,
                    "observes_outcome": true,
                    "injects_context": true,
                    "learns": true,
                }),
            )
            .note(
                "Register under OpenClaw's context-engine lifecycle hooks; \
                 ghost-hook maps ingest->observe_session, assemble->context injection, \
                 after-turn->observe_outcome.",
            )
    }
}
