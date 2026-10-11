//! `ghost-hook`: host-adapter lifecycle hook invocations.
//!
//! Every host adapter's `install()` payload references `ghost-hook
//! <subcommand>` commands. Hosts invoke this binary once per lifecycle
//! event; it reads a JSON session payload from stdin, runs it through a
//! real [`StealthLoop`], and writes a JSON result to stdout.
//!
//! Cross-process continuity is event-sourced: each invocation replays the
//! durable hook journal (`<state_dir>/stealth/hook_events.jsonl`) into a
//! fresh loop before processing, then appends its own record. The journal
//! is append-only; intervention ids are deterministic
//! (`ivn-<stable_id(event_id)>`), so replay rebuilds identical state.
//!
//! Subcommands and their loop mapping (mirrors the Python `ghost-hook`
//! contract referenced by the adapter install payloads):
//! - `openclaw-ingest` / `prompt` / `start` / `gemini-prompt` → observe
//!   session (emit an event through the loop pipeline)
//! - `openclaw-assemble` → context injection (prepared context for the host)
//! - `openclaw-after-turn` / `end` / `gemini-tool` → observe outcome
//! - `mcp` → minimal MCP-over-stdio JSON-RPC 2.0 server exposing the same
//!   three operations as tools (used by the Codex/OpenCode adapters)

use std::io::{BufRead, Read, Write};
use std::path::PathBuf;

use clap::Subcommand;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use ghost_core::GhostConfig;

use crate::event::{new_event, NewEventArgs};
use crate::{InterventionOutcome, StealthLoop};

/// Lifecycle hook subcommands. Clap renames these to kebab-case, matching
/// the exact command strings emitted by the adapter `install()` payloads
/// (e.g. `ghost-hook openclaw-ingest`).
#[derive(Debug, Clone, Subcommand, PartialEq, Eq)]
#[command(rename_all = "kebab-case")]
pub enum HookCommand {
    /// OpenClaw context-engine: observe a session turn.
    OpenclawIngest,
    /// OpenClaw context-engine: assemble prepared context for injection.
    OpenclawAssemble,
    /// OpenClaw context-engine: observe the turn outcome.
    OpenclawAfterTurn,
    /// Claude Code prompt hook: observe the submitted prompt.
    Prompt,
    /// Claude Code session-start hook.
    Start,
    /// Claude Code session-end hook: observe the session outcome.
    End,
    /// Gemini before-prompt hook.
    GeminiPrompt,
    /// Gemini after-tool hook: observe the tool outcome.
    GeminiTool,
    /// MCP bridge over stdio (JSON-RPC 2.0) for the Codex/OpenCode adapters.
    Mcp,
}

impl HookCommand {
    /// Canonical command string, as referenced by install payloads.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::OpenclawIngest => "openclaw-ingest",
            Self::OpenclawAssemble => "openclaw-assemble",
            Self::OpenclawAfterTurn => "openclaw-after-turn",
            Self::Prompt => "prompt",
            Self::Start => "start",
            Self::End => "end",
            Self::GeminiPrompt => "gemini-prompt",
            Self::GeminiTool => "gemini-tool",
            Self::Mcp => "mcp",
        }
    }

    /// True for the observe-session family of hooks.
    fn is_ingest(&self) -> bool {
        matches!(
            self,
            Self::OpenclawIngest | Self::Prompt | Self::Start | Self::GeminiPrompt
        )
    }

    /// True for the observe-outcome family of hooks.
    fn is_after_turn(&self) -> bool {
        matches!(self, Self::OpenclawAfterTurn | Self::End | Self::GeminiTool)
    }
}

/// JSON session payload read from stdin by every hook subcommand.
///
/// All fields are optional; missing fields fall back to documented defaults
/// so hosts can invoke hooks with minimal ceremony.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct SessionPayload {
    /// Host session identifier (used to correlate ingest/assemble/after-turn).
    #[serde(default)]
    pub session_id: String,
    /// Explicit intervention id (assemble/after-turn). When empty, the most
    /// recent intervention for `session_id` is used.
    #[serde(default)]
    pub intervention_id: String,
    /// Event kind: `"prompt"` (default) or `"tool"`.
    #[serde(default)]
    pub kind: String,
    /// The prompt text / tool call description being observed.
    #[serde(default)]
    pub content: String,
    /// Turn outcome for after-turn hooks: true = useful, false/absent = ignored.
    #[serde(default)]
    pub useful: Option<bool>,
    /// Loop confidence override (default 0.9; the loop acts at >= 0.5).
    #[serde(default)]
    pub confidence: Option<f64>,
    /// Event source label (default `"ghost-hook"`).
    #[serde(default)]
    pub source: String,
}

/// A `ghost-hook` invocation: journal-backed [`StealthLoop`] plus helpers.
pub struct HookApp {
    journal_path: PathBuf,
    hook_loop: StealthLoop,
}

impl HookApp {
    /// Open the hook app: ensure state dirs, replay the durable journal into
    /// a fresh loop.
    pub fn open(config: &GhostConfig) -> Result<Self, String> {
        config.ensure_state_dirs().map_err(|e| e.to_string())?;
        let journal_path = config.stealth_dir().join("hook_events.jsonl");
        let app = Self {
            journal_path,
            hook_loop: StealthLoop::new(1024, 256),
        };
        app.replay()?;
        Ok(app)
    }

    /// Event-sourced continuity: re-emit journaled ingest events so the
    /// fresh loop rebuilds the same interventions (ids are deterministic).
    fn replay(&self) -> Result<(), String> {
        let content = std::fs::read_to_string(&self.journal_path).unwrap_or_default();
        for (n, line) in content.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let record: Value =
                serde_json::from_str(line).map_err(|e| format!("journal line {n}: {e}"))?;
            if record.get("op").and_then(|v| v.as_str()) != Some("ingest") {
                continue;
            }
            if let Some(event) = journal_record_to_event(&record) {
                self.hook_loop.emit(event);
            }
        }
        Ok(())
    }

    /// Append one record to the durable hook journal (append-only).
    fn journal_append(&self, record: &Value) -> Result<(), String> {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.journal_path)
            .map_err(|e| format!("open hook journal: {e}"))?;
        let line = serde_json::to_string(record).map_err(|e| e.to_string())?;
        writeln!(file, "{line}").map_err(|e| format!("write hook journal: {e}"))?;
        Ok(())
    }

    /// Observe a session turn: run the payload through the real loop
    /// pipeline (EVENT → … → DECIDE) and journal the record.
    pub fn ingest(&self, subcommand: &str, payload: &SessionPayload) -> Value {
        let confidence = payload.confidence.unwrap_or(0.9).clamp(0.0, 1.0);
        let event_type = match payload.kind.as_str() {
            "tool" => "agent.tool_called",
            _ => "agent.prompt_submitted",
        };
        let source = if payload.source.is_empty() {
            "ghost-hook".to_string()
        } else {
            payload.source.clone()
        };
        let mut args = NewEventArgs::new(source.clone());
        args.session_id = payload.session_id.clone();
        args.confidence = confidence;
        args.payload = json!({
            "content": payload.content,
            "kind": payload.kind,
            "hook": subcommand,
        });
        let event = new_event(event_type, args);
        let event_id = event.event_id.clone();
        let intervention_id = self.hook_loop.emit(event);
        let record = json!({
            "op": "ingest",
            "ts": ghost_core::now(),
            "subcommand": subcommand,
            "session_id": payload.session_id,
            "event_id": event_id,
            "event_type": event_type,
            "source": source,
            "content": payload.content,
            "confidence": confidence,
            "intervention_id": intervention_id,
        });
        if let Err(e) = self.journal_append(&record) {
            return json!({"ok": false, "error": e});
        }
        json!({
            "ok": true,
            "event_id": event_id,
            "intervention_id": intervention_id,
        })
    }

    /// Assemble prepared context for injection into the host prompt.
    ///
    /// Resolves the intervention (explicit id wins, else the latest ingest
    /// for the session), marks it injected with the standing context, and
    /// returns the context. Returns an honest empty context when the loop
    /// decided not to act — never a fabricated one.
    pub fn assemble(&self, payload: &SessionPayload) -> Value {
        let intervention_id = if !payload.intervention_id.is_empty() {
            Some(payload.intervention_id.clone())
        } else {
            self.latest_intervention_for_session(&payload.session_id)
        };
        let Some(id) = intervention_id else {
            return json!({"ok": true, "context": "", "intervention_id": ""});
        };
        let context = self.hook_loop.get_standing_context();
        match self.hook_loop.inject(&id, &context) {
            Ok(()) => {
                let record = json!({
                    "op": "assemble",
                    "ts": ghost_core::now(),
                    "session_id": payload.session_id,
                    "intervention_id": id,
                });
                if let Err(e) = self.journal_append(&record) {
                    return json!({"ok": false, "error": e});
                }
                json!({"ok": true, "context": context, "intervention_id": id})
            }
            Err(e) => json!({"ok": false, "error": e.to_string()}),
        }
    }

    /// Observe the turn outcome: record useful/ignored against the
    /// intervention and journal the record.
    pub fn after_turn(&self, payload: &SessionPayload) -> Value {
        let intervention_id = if !payload.intervention_id.is_empty() {
            Some(payload.intervention_id.clone())
        } else {
            self.latest_intervention_for_session(&payload.session_id)
        };
        let Some(id) = intervention_id else {
            return json!({"ok": false, "error": "no intervention_id and no prior ingest for session"});
        };
        let outcome = match payload.useful {
            Some(true) => InterventionOutcome::Useful,
            _ => InterventionOutcome::Ignored,
        };
        match self.hook_loop.observe_outcome(&id, outcome) {
            Ok(()) => {
                let record = json!({
                    "op": "after_turn",
                    "ts": ghost_core::now(),
                    "session_id": payload.session_id,
                    "intervention_id": id,
                    "outcome": outcome.as_str(),
                });
                if let Err(e) = self.journal_append(&record) {
                    return json!({"ok": false, "error": e});
                }
                json!({"ok": true, "outcome": outcome.as_str(), "intervention_id": id})
            }
            Err(e) => json!({"ok": false, "error": e.to_string()}),
        }
    }

    /// Latest intervention id journaled for a session (any session when the
    /// id is empty).
    fn latest_intervention_for_session(&self, session_id: &str) -> Option<String> {
        let content = std::fs::read_to_string(&self.journal_path).ok()?;
        let mut latest: Option<String> = None;
        for line in content.lines() {
            let record: Value = serde_json::from_str(line.trim()).ok()?;
            if record.get("op").and_then(|v| v.as_str()) != Some("ingest") {
                continue;
            }
            let sid = record
                .get("session_id")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if !session_id.is_empty() && sid != session_id {
                continue;
            }
            if let Some(iid) = record
                .get("intervention_id")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            {
                latest = Some(iid.to_string());
            }
        }
        latest
    }

    /// Handle one MCP-over-stdio JSON-RPC 2.0 request. Returns `None` for
    /// notifications (no response).
    pub fn handle_mcp_request(&self, req: &Value) -> Option<Value> {
        let id = req.get("id").cloned().unwrap_or(Value::Null);
        let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let ok = |result: Value| Some(json!({"jsonrpc": "2.0", "id": id, "result": result}));
        let err = |code: i64, message: &str| {
            Some(json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}))
        };
        match method {
            "initialize" => ok(json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "ghost-hook", "version": env!("CARGO_PKG_VERSION")},
            })),
            "notifications/initialized" => None,
            "tools/list" => ok(json!({"tools": [
                {
                    "name": "ghost.observe_session",
                    "description": "Observe a host session turn through the Stealth Loop.",
                    "inputSchema": {"type": "object", "properties": {
                        "session_id": {"type": "string"},
                        "kind": {"type": "string"},
                        "content": {"type": "string"},
                        "confidence": {"type": "number"},
                    }},
                },
                {
                    "name": "ghost.get_context",
                    "description": "Assemble prepared context for injection into the host prompt.",
                    "inputSchema": {"type": "object", "properties": {
                        "session_id": {"type": "string"},
                        "intervention_id": {"type": "string"},
                    }},
                },
                {
                    "name": "ghost.observe_outcome",
                    "description": "Record the turn outcome (useful/ignored) against an intervention.",
                    "inputSchema": {"type": "object", "properties": {
                        "session_id": {"type": "string"},
                        "intervention_id": {"type": "string"},
                        "useful": {"type": "boolean"},
                    }},
                },
            ]})),
            "tools/call" => {
                let params = req.get("params").cloned().unwrap_or(Value::Null);
                let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                let payload: SessionPayload = serde_json::from_value(args).unwrap_or_default();
                let result = match name {
                    "ghost.observe_session" => self.ingest("mcp", &payload),
                    "ghost.get_context" => self.assemble(&payload),
                    "ghost.observe_outcome" => self.after_turn(&payload),
                    _ => return err(-32602, &format!("unknown tool: {name}")),
                };
                ok(json!({"content": [{"type": "text", "text": result.to_string()}]}))
            }
            _ => err(-32601, &format!("method not found: {method}")),
        }
    }

    /// Run the MCP bridge: line-delimited JSON-RPC 2.0 over stdin/stdout.
    pub fn run_mcp(&self) -> Result<(), String> {
        let stdin = std::io::stdin();
        let mut stdout = std::io::stdout().lock();
        for line in stdin.lock().lines() {
            let line = line.map_err(|e| format!("read stdin: {e}"))?;
            if line.trim().is_empty() {
                continue;
            }
            let req: Value =
                serde_json::from_str(&line).map_err(|e| format!("parse request: {e}"))?;
            if let Some(response) = self.handle_mcp_request(&req) {
                let text = serde_json::to_string(&response).map_err(|e| format!("encode: {e}"))?;
                writeln!(stdout, "{text}").map_err(|e| format!("write stdout: {e}"))?;
                stdout.flush().map_err(|e| format!("flush stdout: {e}"))?;
            }
        }
        Ok(())
    }
}

/// Rebuild a loop [`crate::Event`] from a journaled ingest record so replay
/// regenerates the identical intervention id.
fn journal_record_to_event(record: &Value) -> Option<crate::Event> {
    let get = |k: &str| {
        record
            .get(k)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    let mut args = NewEventArgs::new(get("source"));
    args.session_id = get("session_id");
    args.confidence = record
        .get("confidence")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.9);
    args.event_id = Some(get("event_id")).filter(|s| !s.is_empty());
    args.payload = json!({"content": get("content")});
    Some(new_event(
        record
            .get("event_type")
            .and_then(|v| v.as_str())
            .unwrap_or("agent.prompt_submitted"),
        args,
    ))
}

/// Dispatch one hook invocation: read the JSON session payload from stdin
/// (except `mcp`, which speaks JSON-RPC on the stream), run it, print the
/// JSON result to stdout. Returns the process exit code.
pub fn run(command: HookCommand, config: &GhostConfig) -> i32 {
    let app = match HookApp::open(config) {
        Ok(app) => app,
        Err(e) => {
            eprintln!("{}", json!({"ok": false, "error": e}));
            return 1;
        }
    };
    if command == HookCommand::Mcp {
        return match app.run_mcp() {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("{}", json!({"ok": false, "error": e}));
                1
            }
        };
    }
    let mut input = String::new();
    if let Err(e) = std::io::stdin().read_to_string(&mut input) {
        eprintln!(
            "{}",
            json!({"ok": false, "error": format!("read stdin: {e}")})
        );
        return 1;
    }
    let payload: SessionPayload = if input.trim().is_empty() {
        SessionPayload::default()
    } else {
        match serde_json::from_str(&input) {
            Ok(p) => p,
            Err(e) => {
                eprintln!(
                    "{}",
                    json!({"ok": false, "error": format!("parse payload: {e}")})
                );
                return 1;
            }
        }
    };
    let result = if command.is_ingest() {
        app.ingest(command.as_str(), &payload)
    } else if command == HookCommand::OpenclawAssemble {
        app.assemble(&payload)
    } else if command.is_after_turn() {
        app.after_turn(&payload)
    } else {
        // Unreachable: every variant is classified above.
        json!({"ok": false, "error": format!("unhandled subcommand: {}", command.as_str())})
    };
    println!("{}", serde_json::to_string(&result).unwrap());
    if result.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
        0
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_config() -> (TempDir, GhostConfig) {
        let dir = TempDir::new().unwrap();
        let state = dir.path().join("state");
        std::env::set_var("GHOSTCHIMERA_STATE_DIR", &state);
        let config = GhostConfig::from_env();
        std::env::remove_var("GHOSTCHIMERA_STATE_DIR");
        (dir, config)
    }

    fn payload(session: &str, content: &str) -> SessionPayload {
        SessionPayload {
            session_id: session.to_string(),
            content: content.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn hook_command_strings_match_install_payloads() {
        // Every command string referenced by adapter install() payloads must
        // exist here. (mod.rs tests assert install payloads start with
        // "ghost-hook"; this asserts the subcommand half resolves.)
        for cmd in [
            HookCommand::OpenclawIngest,
            HookCommand::OpenclawAssemble,
            HookCommand::OpenclawAfterTurn,
            HookCommand::Prompt,
            HookCommand::Start,
            HookCommand::End,
            HookCommand::GeminiPrompt,
            HookCommand::GeminiTool,
            HookCommand::Mcp,
        ] {
            assert!(!cmd.as_str().is_empty());
        }
        assert_eq!(HookCommand::OpenclawIngest.as_str(), "openclaw-ingest");
        assert_eq!(HookCommand::Mcp.as_str(), "mcp");
    }

    #[test]
    fn ingest_runs_real_pipeline_and_journals() {
        let (_dir, config) = test_config();
        let app = HookApp::open(&config).unwrap();
        let result = app.ingest("openclaw-ingest", &payload("s1", "hello ghost"));
        assert_eq!(result["ok"], true);
        assert!(result["event_id"].as_str().unwrap().starts_with("evt-"));
        // High-confidence prompt → the loop really decided to act.
        let iid = result["intervention_id"].as_str().unwrap();
        assert!(!iid.is_empty());
        assert!(app.hook_loop.intervention(iid).is_some());
        // Journaled durably.
        let journal =
            std::fs::read_to_string(config.stealth_dir().join("hook_events.jsonl")).unwrap();
        assert!(journal.contains("\"op\":\"ingest\""));
        assert!(journal.contains("hello ghost"));
    }

    #[test]
    fn assemble_returns_context_after_ingest() {
        let (_dir, config) = test_config();
        let app = HookApp::open(&config).unwrap();
        let ingested = app.ingest("prompt", &payload("s2", "what is the plan"));
        let iid = ingested["intervention_id"].as_str().unwrap().to_string();

        let result = app.assemble(&payload("s2", ""));
        assert_eq!(result["ok"], true);
        assert_eq!(result["intervention_id"].as_str().unwrap(), iid);
        // Standing context reflects the observed event — real, not fabricated.
        assert!(result["context"]
            .as_str()
            .unwrap()
            .contains("agent.prompt_submitted"));
        // The intervention is now marked injected.
        let ivn = app.hook_loop.intervention(&iid).unwrap();
        assert_eq!(ivn.context, result["context"].as_str().unwrap());
    }

    #[test]
    fn assemble_without_prior_ingest_is_honestly_empty() {
        let (_dir, config) = test_config();
        let app = HookApp::open(&config).unwrap();
        let result = app.assemble(&payload("nope", ""));
        assert_eq!(result["ok"], true);
        assert_eq!(result["context"], "");
        assert_eq!(result["intervention_id"], "");
    }

    #[test]
    fn after_turn_records_real_outcome() {
        let (_dir, config) = test_config();
        let app = HookApp::open(&config).unwrap();
        let ingested = app.ingest("openclaw-ingest", &payload("s3", "do a thing"));
        let iid = ingested["intervention_id"].as_str().unwrap().to_string();

        let mut p = payload("s3", "");
        p.useful = Some(true);
        let result = app.after_turn(&p);
        assert_eq!(result["ok"], true);
        assert_eq!(result["outcome"], "useful");
        let outcomes = app.hook_loop.outcomes();
        assert!(outcomes
            .iter()
            .any(|(id, o)| id == &iid && *o == InterventionOutcome::Useful));
    }

    #[test]
    fn after_turn_without_intervention_is_honest_error() {
        let (_dir, config) = test_config();
        let app = HookApp::open(&config).unwrap();
        let result = app.after_turn(&payload("ghost-session", ""));
        assert_eq!(result["ok"], false);
        assert!(result["error"]
            .as_str()
            .unwrap()
            .contains("no intervention_id"));
    }

    #[test]
    fn journal_replay_gives_cross_invocation_continuity() {
        let (_dir, config) = test_config();
        // First "process": ingest.
        let iid = {
            let app = HookApp::open(&config).unwrap();
            let r = app.ingest("openclaw-ingest", &payload("s4", "remember this"));
            r["intervention_id"].as_str().unwrap().to_string()
        };
        // Second "process": fresh app replays the journal; assemble works.
        {
            let app = HookApp::open(&config).unwrap();
            let r = app.assemble(&payload("s4", ""));
            assert_eq!(r["ok"], true);
            assert_eq!(r["intervention_id"].as_str().unwrap(), iid);
        }
    }

    #[test]
    fn low_confidence_ingest_may_not_create_intervention() {
        let (_dir, config) = test_config();
        let app = HookApp::open(&config).unwrap();
        let mut p = payload("s5", "background noise");
        p.confidence = Some(0.1);
        let result = app.ingest("prompt", &p);
        assert_eq!(result["ok"], true);
        // The loop honestly stayed silent; no fabricated intervention.
        assert!(result["intervention_id"]
            .as_str()
            .unwrap_or_default()
            .is_empty());
    }

    #[test]
    fn mcp_initialize_and_tools_list() {
        let (_dir, config) = test_config();
        let app = HookApp::open(&config).unwrap();
        let init = app
            .handle_mcp_request(&json!({"jsonrpc": "2.0", "id": 1, "method": "initialize"}))
            .unwrap();
        assert_eq!(init["result"]["serverInfo"]["name"], "ghost-hook");

        let list = app
            .handle_mcp_request(&json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}))
            .unwrap();
        let names: Vec<&str> = list["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"ghost.observe_session"));
        assert!(names.contains(&"ghost.get_context"));
        assert!(names.contains(&"ghost.observe_outcome"));
    }

    #[test]
    fn mcp_tools_call_dispatches_to_real_loop() {
        let (_dir, config) = test_config();
        let app = HookApp::open(&config).unwrap();
        let call = json!({
            "jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": {"name": "ghost.observe_session",
                       "arguments": {"session_id": "mcp1", "content": "via mcp"}},
        });
        let resp = app.handle_mcp_request(&call).unwrap();
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        let inner: Value = serde_json::from_str(text).unwrap();
        assert_eq!(inner["ok"], true);
        assert!(!inner["intervention_id"].as_str().unwrap().is_empty());

        // Unknown tool → JSON-RPC invalid params, not a panic.
        let bad = json!({
            "jsonrpc": "2.0", "id": 4, "method": "tools/call",
            "params": {"name": "ghost.nope", "arguments": {}},
        });
        let resp = app.handle_mcp_request(&bad).unwrap();
        assert_eq!(resp["error"]["code"], -32602);

        // Unknown method → method not found.
        let resp = app
            .handle_mcp_request(&json!({"jsonrpc": "2.0", "id": 5, "method": "bogus"}))
            .unwrap();
        assert_eq!(resp["error"]["code"], -32601);

        // Notifications get no response.
        assert!(app
            .handle_mcp_request(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
            .is_none());
    }
}
