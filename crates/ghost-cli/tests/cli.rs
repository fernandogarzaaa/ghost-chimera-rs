//! End-to-end tests of the real `ghost` binary: argument parsing, dispatch,
//! output and exit codes.
//!
//! Hermetic: each run gets a cleared environment (no provider keys, no
//! proxies), a fresh `HOME`, and a temporary `GHOSTCHIMERA_STATE_DIR`, so
//! results never depend on the developer's shell and nothing touches the
//! network (the offline deterministic backend handles every objective).

use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

fn ghost(home: &Path, args: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_ghost"));
    cmd.env_clear()
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("GHOSTCHIMERA_STATE_DIR", home.join("state"))
        .args(args);
    if let Some(path) = std::env::var_os("PATH") {
        cmd.env("PATH", path);
    }
    // Windows needs SystemRoot to initialise networking/crypto APIs.
    if let Some(root) = std::env::var_os("SystemRoot") {
        cmd.env("SystemRoot", root);
    }
    cmd.output().expect("run ghost binary")
}

fn stdout_json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|err| {
        panic!(
            "stdout is not JSON ({err}): {}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

#[test]
fn ask_without_objective_exits_2_with_error_on_stderr() {
    let home = TempDir::new().unwrap();
    let out = ghost(home.path(), &["ask"]);
    assert_eq!(out.status.code(), Some(2));
    let err: Value = serde_json::from_slice(&out.stderr).expect("stderr JSON");
    assert_eq!(err["ok"], false);
    assert_eq!(err["error"], "objective is required");
}

#[test]
fn ask_fulfillable_objective_exits_0_and_prints_reply() {
    let home = TempDir::new().unwrap();
    let out = ghost(home.path(), &["ask", "what", "is", "2", "+", "2"]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains('4'), "expected the answer in: {text}");
}

#[test]
fn ask_unfulfillable_objective_exits_1_with_honest_json() {
    let home = TempDir::new().unwrap();
    let out = ghost(
        home.path(),
        &[
            "ask", "--json", "write", "a", "poem", "about", "the", "ocean",
        ],
    );
    assert_eq!(out.status.code(), Some(1));
    let value = stdout_json(&out);
    assert_eq!(value["ok"], false);
    assert!(
        value["trust_run_id"]
            .as_str()
            .is_some_and(|id| !id.is_empty()),
        "run must be journaled: {value}"
    );
}

#[test]
fn trust_replay_unknown_run_exits_1() {
    let home = TempDir::new().unwrap();
    let out = ghost(home.path(), &["trust", "replay", "no-such-run"]);
    assert_eq!(out.status.code(), Some(1));
    let value = stdout_json(&out);
    assert_eq!(value["ok"], false);
    assert!(
        value["error"].is_string(),
        "error message expected: {value}"
    );
}

#[test]
fn spend_show_exits_0_with_ledger_snapshot() {
    let home = TempDir::new().unwrap();
    let out = ghost(home.path(), &["spend", "show"]);
    assert_eq!(out.status.code(), Some(0));
    let value = stdout_json(&out);
    assert_eq!(value["ok"], true);
    assert!(value["providers"].is_object() || value["providers"].is_array());
}
