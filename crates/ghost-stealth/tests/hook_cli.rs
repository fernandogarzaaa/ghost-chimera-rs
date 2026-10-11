//! End-to-end tests for the `ghost-hook` binary: real subprocess invocations
//! with a fresh state dir, exercising the full ingest → assemble →
//! after-turn lifecycle across process boundaries.

use std::io::Write;
use std::process::{Command, Stdio};

use serde_json::{json, Value};
use tempfile::TempDir;

fn hook_bin() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_ghost-hook"))
}

/// Run `ghost-hook <subcommand>` with the given JSON payload on stdin.
/// Returns (exit_code, parsed stdout JSON).
fn run_hook(dir: &TempDir, subcommand: &str, payload: &Value) -> (i32, Value) {
    let mut child = Command::new(hook_bin())
        .arg(subcommand)
        .env("GHOSTCHIMERA_STATE_DIR", dir.path().join("state"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn ghost-hook");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(payload.to_string().as_bytes())
        .unwrap();
    let output = child.wait_with_output().expect("wait ghost-hook");
    let code = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let value: Value = serde_json::from_str(stdout.trim()).expect("hook stdout must be JSON");
    (code, value)
}

#[test]
fn full_lifecycle_across_processes() {
    let dir = TempDir::new().unwrap();

    // 1. Ingest (separate process).
    let (code, ingested) = run_hook(
        &dir,
        "openclaw-ingest",
        &json!({"session_id": "e2e1", "content": "plan the release"}),
    );
    assert_eq!(code, 0, "ingest stdout: {ingested}");
    assert_eq!(ingested["ok"], true);
    let iid = ingested["intervention_id"].as_str().unwrap().to_string();
    assert!(!iid.is_empty());

    // 2. Assemble in a NEW process: journal replay must restore continuity.
    let (code, assembled) = run_hook(&dir, "openclaw-assemble", &json!({"session_id": "e2e1"}));
    assert_eq!(code, 0, "assemble stdout: {assembled}");
    assert_eq!(assembled["ok"], true);
    assert_eq!(assembled["intervention_id"].as_str().unwrap(), iid);
    assert!(assembled["context"]
        .as_str()
        .unwrap()
        .contains("agent.prompt_submitted"));

    // 3. After-turn in another new process.
    let (code, done) = run_hook(
        &dir,
        "openclaw-after-turn",
        &json!({"session_id": "e2e1", "useful": true}),
    );
    assert_eq!(code, 0, "after-turn stdout: {done}");
    assert_eq!(done["ok"], true);
    assert_eq!(done["outcome"], "useful");
}

#[test]
fn claude_code_hooks_work() {
    let dir = TempDir::new().unwrap();
    let (code, out) = run_hook(&dir, "prompt", &json!({"content": "hello"}));
    assert_eq!(code, 0);
    assert_eq!(out["ok"], true);

    let (code, out) = run_hook(&dir, "start", &json!({}));
    assert_eq!(code, 0);
    assert_eq!(out["ok"], true);

    let (code, out) = run_hook(&dir, "end", &json!({"useful": false}));
    assert_eq!(code, 0);
    // "end" with no session correlation still resolves the latest intervention.
    assert_eq!(out["ok"], true);
    assert_eq!(out["outcome"], "ignored");
}

#[test]
fn gemini_hooks_work() {
    let dir = TempDir::new().unwrap();
    let (code, out) = run_hook(
        &dir,
        "gemini-prompt",
        &json!({"session_id": "g1", "content": "summarize"}),
    );
    assert_eq!(code, 0);
    assert_eq!(out["ok"], true);

    let (code, out) = run_hook(
        &dir,
        "gemini-tool",
        &json!({"session_id": "g1", "useful": true}),
    );
    assert_eq!(code, 0);
    assert_eq!(out["ok"], true);
    assert_eq!(out["outcome"], "useful");
}

#[test]
fn invalid_payload_is_honest_error() {
    let dir = TempDir::new().unwrap();
    let mut child = Command::new(hook_bin())
        .arg("openclaw-ingest")
        .env("GHOSTCHIMERA_STATE_DIR", dir.path().join("state"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"this is not json")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert_ne!(output.status.code().unwrap(), 0);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("\"ok\":false"), "stderr: {stderr}");
}

#[test]
fn mcp_initialize_roundtrip() {
    let dir = TempDir::new().unwrap();
    let mut child = Command::new(hook_bin())
        .arg("mcp")
        .env("GHOSTCHIMERA_STATE_DIR", dir.path().join("state"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let stdin = child.stdin.as_mut().unwrap();
    writeln!(
        stdin,
        "{}",
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}})
    )
    .unwrap();
    writeln!(
        stdin,
        "{}",
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
               "params": {"name": "ghost.observe_session",
                          "arguments": {"session_id": "mcp-e2e", "content": "hi"}}})
    )
    .unwrap();
    drop(child.stdin.take());
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<Value> = stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["result"]["serverInfo"]["name"], "ghost-hook");
    let inner: Value =
        serde_json::from_str(lines[1]["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(inner["ok"], true);
}
