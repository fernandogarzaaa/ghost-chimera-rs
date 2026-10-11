//! Host adapters: Ghost's universal event model <-> host-native lifecycles.
//!
//! Mirrors `ghostchimera/stealth/hosts.py`. The [`HostAdapter`] trait has
//! **no default `install()`** — every adapter must return a real config
//! payload (bug #1 made the stub unrepresentable at compile time).

mod claude_code;
mod codex;
mod gemini;
mod hermes;
mod openclaw;
mod opencode;

pub use claude_code::ClaudeCodeAdapter;
pub use codex::CodexAdapter;
pub use gemini::GeminiAdapter;
pub use hermes::HermesAdapter;
pub use openclaw::OpenClawAdapter;
pub use opencode::OpenCodeAdapter;

use std::collections::HashMap;

/// Host-native install artifacts: config snippets and scripts.
///
/// Serializes to the same shape as the Python `install()` dicts so
/// console tooling can consume either implementation.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct InstallPayload {
    pub host: String,
    pub installed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Arbitrary host-native config (settings snippets, hooks, plugins…).
    #[serde(flatten)]
    pub artifacts: HashMap<String, serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl InstallPayload {
    pub fn installed(host: &str) -> Self {
        Self {
            host: host.to_string(),
            installed: true,
            reason: None,
            artifacts: HashMap::new(),
            note: None,
        }
    }

    pub fn artifact(mut self, key: &str, value: serde_json::Value) -> Self {
        self.artifacts.insert(key.to_string(), value);
        self
    }

    pub fn note(mut self, note: &str) -> Self {
        self.note = Some(note.to_string());
        self
    }
}

/// Universal host contract.
///
/// NOTE: `install()` has no default implementation on purpose — the
/// OpenClaw stub (bug #1) must be unrepresentable.
pub trait HostAdapter: Send + Sync {
    /// Stable host id (`"openclaw"`, `"claude-code"`, …).
    fn id(&self) -> &'static str;
    /// Human-readable name.
    fn name(&self) -> &'static str;
    /// True when this host's CLI/config is present on this machine.
    fn detect(&self) -> bool;
    /// Return host-native install artifacts (config snippets, scripts).
    fn install(&self) -> InstallPayload;
}

/// Registry of all known adapters.
#[derive(Default)]
pub struct AdapterRegistry {
    adapters: Vec<Box<dyn HostAdapter>>,
}

impl AdapterRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register every built-in adapter.
    pub fn with_all() -> Self {
        let mut reg = Self::new();
        reg.register(Box::new(ClaudeCodeAdapter));
        reg.register(Box::new(OpenClawAdapter));
        reg.register(Box::new(OpenCodeAdapter));
        reg.register(Box::new(CodexAdapter));
        reg.register(Box::new(GeminiAdapter));
        reg.register(Box::new(HermesAdapter));
        reg
    }

    pub fn register(&mut self, adapter: Box<dyn HostAdapter>) {
        self.adapters.push(adapter);
    }

    pub fn get(&self, id: &str) -> Option<&dyn HostAdapter> {
        self.adapters
            .iter()
            .find(|a| a.id() == id)
            .map(|a| a.as_ref())
    }

    pub fn ids(&self) -> Vec<&'static str> {
        self.adapters.iter().map(|a| a.id()).collect()
    }

    /// Adapters whose host is detected on this machine.
    pub fn detected(&self) -> Vec<&'static str> {
        self.adapters
            .iter()
            .filter(|a| a.detect())
            .map(|a| a.id())
            .collect()
    }
}

/// True when `name` resolves on PATH.
fn which(name: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths)
                .any(|dir| dir.join(name).is_file() || dir.join(format!("{name}.exe")).is_file())
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_holds_all_six() {
        let reg = AdapterRegistry::with_all();
        assert_eq!(reg.ids().len(), 6);
        for id in [
            "claude-code",
            "openclaw",
            "opencode",
            "codex",
            "gemini",
            "hermes",
        ] {
            assert!(reg.get(id).is_some(), "missing {id}");
        }
    }

    #[test]
    fn every_adapter_install_reports_installed() {
        // Bug #1 regression: no adapter may return installed=false.
        let reg = AdapterRegistry::with_all();
        for id in reg.ids() {
            let payload = reg.get(id).unwrap().install();
            assert!(payload.installed, "{id} must not be a stub");
            assert_eq!(payload.host, id);
        }
    }

    #[test]
    fn openclaw_install_returns_lifecycle_hooks() {
        // Exact port of the Python PR #117 assertion.
        let payload = OpenClawAdapter.install();
        assert!(payload.installed);
        let snippet = payload
            .artifacts
            .get("context_engine_snippet")
            .expect("context_engine_snippet");
        let hooks = snippet.get("lifecycle_hooks").expect("lifecycle_hooks");
        for hook in ["ingest", "assemble", "after-turn"] {
            let cmd = hooks
                .get(hook)
                .and_then(|h| h.get("command"))
                .and_then(|c| c.as_str())
                .unwrap_or("");
            assert!(
                cmd.starts_with("ghost-hook"),
                "hook {hook} command must be a ghost-hook invocation, got {cmd:?}"
            );
        }
        let caps = payload.artifacts.get("capabilities").expect("capabilities");
        assert_eq!(caps["observes_outcome"], serde_json::Value::Bool(true));
        assert_eq!(caps["learns"], serde_json::Value::Bool(true));
    }
}
