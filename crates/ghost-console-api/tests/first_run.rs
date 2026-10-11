//! First-run experience: `ghost console` with zero configuration must work.
//!
//! Spins up the real console HTTP server against a fresh state dir with no
//! provider credentials in the environment, then drives it over real TCP:
//! status must report ok, and a conversation turn must be fulfilled by the
//! offline deterministic backend with an honest note (never a fabricated
//! model reply).

use std::net::SocketAddr;
use std::time::Duration;

use ghost_console_api::{router, AppState};
use ghost_core::GhostConfig;
use serde_json::{json, Value};
use tempfile::TempDir;

/// Provider credential env vars that must be absent for a true zero-config run.
const PROVIDER_VARS: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "OPENAI_API_KEY",
    "NEBIUS_API_KEY",
    "GROQ_API_KEY",
    "GHOSTCHIMERA_MODEL_PROVIDER",
];

struct LiveServer {
    _dir: TempDir,
    base_url: String,
    saved_vars: Vec<(String, Option<String>)>,
    saved_state_dir: Option<String>,
}

impl LiveServer {
    async fn start() -> Self {
        let dir = TempDir::new().unwrap();
        let state_dir = dir.path().join("state");

        // Zero configuration: fresh state dir, no provider keys.
        let saved_state_dir = std::env::var("GHOSTCHIMERA_STATE_DIR").ok();
        std::env::set_var("GHOSTCHIMERA_STATE_DIR", &state_dir);
        let mut saved_vars = Vec::new();
        for var in PROVIDER_VARS {
            let saved = std::env::var(var).ok();
            std::env::remove_var(var);
            saved_vars.push((var.to_string(), saved));
        }

        let config = GhostConfig::from_env();
        config.ensure_state_dirs().unwrap();
        // First run: nothing to migrate, but the call must be a safe no-op.
        assert!(!config.migrate_legacy_state().unwrap());

        let state = AppState::new(config, None).unwrap();
        let (app, _routes) = router(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr: SocketAddr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        // Give the server a moment to accept connections.
        tokio::time::sleep(Duration::from_millis(100)).await;

        Self {
            _dir: dir,
            base_url: format!("http://{addr}"),
            saved_vars,
            saved_state_dir,
        }
    }

    fn client(&self) -> reqwest::Client {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap()
    }
}

impl Drop for LiveServer {
    fn drop(&mut self) {
        for (var, saved) in self.saved_vars.drain(..) {
            match saved {
                Some(v) => std::env::set_var(&var, v),
                None => std::env::remove_var(&var),
            }
        }
        match self.saved_state_dir.take() {
            Some(v) => std::env::set_var("GHOSTCHIMERA_STATE_DIR", v),
            None => std::env::remove_var("GHOSTCHIMERA_STATE_DIR"),
        }
    }
}

#[tokio::test]
async fn first_run_status_is_ok_with_zero_config() {
    let server = LiveServer::start().await;
    let body: Value = server
        .client()
        .get(format!("{}/api/console/status", server.base_url))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["ok"], true);
    assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));
}

#[tokio::test]
async fn first_run_conversation_uses_honest_deterministic_backend() {
    let server = LiveServer::start().await;
    let body: Value = server
        .client()
        .post(format!("{}/api/conversation/message", server.base_url))
        .json(&json!({"message": "what is 6 * 7"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["ok"], true);
    // The deterministic backend really computed this.
    assert_eq!(body["reply"], "42");
    // ...and honestly said so.
    let note = body["note"].as_str().expect("honest offline note");
    assert!(
        note.contains("offline deterministic backend"),
        "note was: {note}"
    );
    // The run was journaled.
    assert!(body["trust_run_id"].as_str().is_some());
}

#[tokio::test]
async fn first_run_unfulfillable_objective_is_honest_failure() {
    let server = LiveServer::start().await;
    let body: Value = server
        .client()
        .post(format!("{}/api/conversation/message", server.base_url))
        .json(&json!({"message": "write a poem about the ocean"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    // Honest failure, not a fabricated poem.
    assert_eq!(body["ok"], false);
    let text = body.to_string();
    assert!(
        text.contains("deterministic_unfulfilled"),
        "response was: {text}"
    );
}
