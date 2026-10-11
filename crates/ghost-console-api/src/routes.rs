//! HTTP route handlers: status, conversation, trust, standing orders.
//!
//! Also hosts [`build_kernel`], the shared kernel builder used by the
//! conversation loop, replay endpoint, and the CLI.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use ghost_core::GhostError;
use ghost_pilot::{ChimeraPilotKernel, KernelOptions};
use ghost_providers::ProviderRegistry;
use ghost_trust::{RecordStepArgs, ReplayExecution};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::state::AppState;

/// Build the pilot kernel for conversation/replay work.
///
/// Uses the first available model provider from the environment; when none
/// is configured, falls back to the offline deterministic backend and
/// returns an honest `note` saying so (bug #5: never claim a model ran).
///
/// NOTE: `ProviderRegistry::from_env()` eagerly constructs reqwest
/// *blocking* clients, which panic when built inside a tokio async
/// context. Call [`build_kernel_async`] from async code instead.
pub fn build_kernel(autonomy_level: &str) -> (ChimeraPilotKernel, Option<String>) {
    let registry = ProviderRegistry::from_env();
    let provider = registry
        .available_names()
        .first()
        .and_then(|name| registry.get(name));
    let options = KernelOptions {
        include_deterministic_backend: true,
        deterministic_fulfill: true,
        include_model_provider_backend: provider.is_some(),
        autonomy_level: autonomy_level.to_string(),
    };
    let kernel = ChimeraPilotKernel::default_with(options, provider.clone());
    let note = if provider.is_some() {
        None
    } else {
        Some(OFFLINE_NOTE.to_string())
    };
    (kernel, note)
}

/// Build a kernel that never touches the network: deterministic backend
/// only, regardless of which provider keys are in the environment.
///
/// Used by tests (so results never depend on the developer's shell) and
/// available to callers that must stay offline.
pub fn build_offline_kernel(autonomy_level: &str) -> ChimeraPilotKernel {
    let options = KernelOptions {
        include_deterministic_backend: true,
        deterministic_fulfill: true,
        include_model_provider_backend: false,
        autonomy_level: autonomy_level.to_string(),
    };
    ChimeraPilotKernel::default_with(options, None)
}

/// Offline note shared by every deterministic-only path.
const OFFLINE_NOTE: &str =
    "Ran on the offline deterministic backend (no model provider configured).";

/// Kernel for a request: offline when the state demands it, else
/// [`build_kernel_async`].
async fn kernel_for(state: &AppState) -> (ChimeraPilotKernel, Option<String>) {
    if state.offline {
        (
            build_offline_kernel(&state.config.autonomy_level),
            Some(OFFLINE_NOTE.to_string()),
        )
    } else {
        build_kernel_async(&state.config.autonomy_level).await
    }
}

/// Async wrapper around [`build_kernel`]: constructs the kernel on a
/// blocking thread, because reqwest blocking clients cannot be built
/// inside a tokio async context (they panic dropping their internal
/// runtime there).
pub async fn build_kernel_async(autonomy_level: &str) -> (ChimeraPilotKernel, Option<String>) {
    let autonomy_level = autonomy_level.to_string();
    tokio::task::spawn_blocking(move || build_kernel(&autonomy_level))
        .await
        .expect("kernel construction is infallible on a blocking thread")
}

// -- status ------------------------------------------------------------

pub async fn status_handler(
    State(state): State<AppState>,
    axum::Extension(route_count): axum::Extension<usize>,
) -> Json<Value> {
    let mode = if state.token.as_deref().unwrap_or("").is_empty() {
        "open"
    } else {
        "token"
    };
    Json(json!({
        "ok": true,
        "version": state.version,
        "routes": route_count,
        "mode": mode,
    }))
}

// -- conversation ------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct MessageRequest {
    pub message: String,
}

/// Run one conversational turn: journal the run, execute the objective,
/// journal per-task steps, and reply honestly.
///
/// The envelope contains EXACTLY ONE top-level `reply` field (bug #2: no
/// duplicated/derived reply copies such as an operator_report echo).
pub async fn conversation_message(
    State(state): State<AppState>,
    Json(body): Json<MessageRequest>,
) -> Json<Value> {
    let objective = body.message.trim().to_string();
    if objective.is_empty() {
        return Json(json!({"ok": false, "error": "message is required"}));
    }
    let run = match state
        .trust
        .create_run(&objective, "conversation", "ghost-console", None)
    {
        Ok(run) => run,
        Err(err) => return Json(json!({"ok": false, "error": err.to_string()})),
    };
    let (kernel, note) = kernel_for(&state).await;
    let executions = match kernel.run(&objective) {
        Ok(executions) => executions,
        Err(err) => {
            return Json(json!({
                "ok": false,
                "error": err.to_string(),
                "trust_run_id": run.run_id,
            }))
        }
    };
    for exec in &executions {
        let output: String = exec.output.chars().take(2000).collect();
        let args = RecordStepArgs {
            step_type: "task_executed",
            status: if exec.ok { "ok" } else { "error" },
            input_payload: Some(json!({
                "task_id": exec.task_id,
                "backend_id": exec.backend_id,
            })),
            output_payload: Some(json!({
                "ok": exec.ok,
                "output": output,
                "error": exec.error,
            })),
            policy_decision: None,
            idempotency_key: None,
            retryable: false,
            duration_ms: 0.0,
            error: &exec.error,
        };
        if state.trust.record_step(&run.run_id, args).is_err() {
            break;
        }
    }
    // Honest reply: first fulfilled execution's output, else the
    // machine-readable failure reason (never a fabricated success).
    let reply = match executions.iter().find(|e| e.ok) {
        Some(exec) => exec.output.clone(),
        None => {
            let reason = executions
                .first()
                .map(|e| e.error.clone())
                .unwrap_or_else(|| "no executions were recorded".to_string());
            format!("I could not complete that request: {reason}")
        }
    };
    let mut envelope = serde_json::Map::with_capacity(5);
    // Honest envelope: ok is true only when every execution fulfilled the
    // objective. An unfulfillable request is ok:false with a
    // deterministic_unfulfilled reason — never a silent success.
    let all_ok = executions.iter().all(|e| e.ok);
    envelope.insert("ok".to_string(), json!(all_ok));
    envelope.insert("reply".to_string(), json!(reply));
    envelope.insert("trust_run_id".to_string(), json!(run.run_id));
    envelope.insert("executions".to_string(), json!(executions));
    if let Some(note) = note {
        envelope.insert("note".to_string(), json!(note));
    }
    Json(Value::Object(envelope))
}

// -- trust -------------------------------------------------------------

pub async fn trust_list_runs(State(state): State<AppState>) -> Response {
    match state.trust.list_runs() {
        Ok(runs) => Json(json!({"ok": true, "runs": runs})).into_response(),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "error": err.to_string()})),
        )
            .into_response(),
    }
}

pub async fn trust_get_run(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    match state.trust.get_run(&id) {
        Ok(Some(view)) => {
            Json(json!({"ok": true, "run": view.run, "steps": view.steps})).into_response()
        }
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({"ok": false, "error": "run not found"})),
        )
            .into_response(),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "error": err.to_string()})),
        )
            .into_response(),
    }
}

/// Replay a journaled run through the kernel and return the comparison.
pub async fn trust_replay(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let (kernel, _note) = kernel_for(&state).await;
    let report = state.trust.replay_run(&id, &|objective| {
        kernel.run(objective).map(|executions| {
            executions
                .into_iter()
                .map(|e| ReplayExecution {
                    task_id: e.task_id,
                    backend_id: e.backend_id,
                    ok: e.ok,
                    output: e.output,
                    error: e.error,
                })
                .collect()
        })
    });
    match report {
        Ok(report) => Json(json!({
            "ok": true,
            "replay_run_id": report.replay_run_id,
            "replayed_from": report.replayed_from,
            "objective": report.objective,
            "execution_performed": report.execution_performed,
            "comparison": report.comparison,
            "executions": report.executions,
        }))
        .into_response(),
        Err(GhostError::NotFound(msg)) => (
            StatusCode::NOT_FOUND,
            Json(json!({"ok": false, "error": msg})),
        )
            .into_response(),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "error": err.to_string()})),
        )
            .into_response(),
    }
}

// -- standing orders ---------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreateOrderRequest {
    pub title: String,
    pub objective: String,
    pub scope: Option<String>,
}

pub async fn standing_orders_list(State(state): State<AppState>) -> Response {
    match state.orders.list_orders() {
        Ok((orders, counts)) => {
            Json(json!({"ok": true, "orders": orders, "counts": counts})).into_response()
        }
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "error": err.to_string()})),
        )
            .into_response(),
    }
}

pub async fn standing_orders_create(
    State(state): State<AppState>,
    Json(body): Json<CreateOrderRequest>,
) -> Response {
    match state
        .orders
        .create_order(&body.title, &body.objective, body.scope.as_deref())
    {
        Ok(order) => Json(json!({"ok": true, "order": order})).into_response(),
        Err(GhostError::Config(msg)) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "error": msg})),
        )
            .into_response(),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "error": err.to_string()})),
        )
            .into_response(),
    }
}

pub async fn standing_orders_enable(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Response {
    set_enabled_response(&state, &id, true)
}

pub async fn standing_orders_disable(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Response {
    set_enabled_response(&state, &id, false)
}

fn set_enabled_response(state: &AppState, id: &str, enabled: bool) -> Response {
    match state.orders.set_enabled(id, enabled) {
        Ok(order) => Json(json!({"ok": true, "order": order})).into_response(),
        Err(GhostError::NotFound(msg)) => (
            StatusCode::NOT_FOUND,
            Json(json!({"ok": false, "error": msg})),
        )
            .into_response(),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "error": err.to_string()})),
        )
            .into_response(),
    }
}

/// Run a standing order. The HTTP surface has no runner, so this is always
/// an honest preview (`executed: false`, no counter changes — HON-F2).
pub async fn standing_orders_run(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Response {
    match state.orders.run_order(&id, None) {
        Ok(outcome) => Json(json!({
            "ok": true,
            "executed": outcome.executed,
            "preview": !outcome.executed,
            "objective": outcome.order.objective,
            "order": outcome.order,
        }))
        .into_response(),
        Err(GhostError::NotFound(msg)) => (
            StatusCode::NOT_FOUND,
            Json(json!({"ok": false, "error": msg})),
        )
            .into_response(),
        Err(err) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "error": err.to_string()})),
        )
            .into_response(),
    }
}

// -- static ------------------------------------------------------------

/// Serve the console `index.html`.
pub async fn index_html() -> Response {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("static/index.html");
    match tokio::fs::read(&path).await {
        Ok(bytes) => (
            [(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")],
            bytes,
        )
            .into_response(),
        Err(_) => (
            StatusCode::NOT_FOUND,
            Json(json!({"ok": false, "error": "index.html not found"})),
        )
            .into_response(),
    }
}

/// Serve a file from this crate's `./static` directory.
///
/// Implemented directly instead of `tower_http::services::ServeDir`:
/// the sandbox has no reliable crates.io egress for tower-http 0.5's
/// `fs`-feature transitive deps (`http-range-header`, `mime_guess`), so
/// static serving is a real handler here. Path traversal is rejected and
/// unknown files 404.
pub async fn static_file(Path(path): Path<String>) -> Response {
    let base = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("static");
    let mut full = base.clone();
    let mut rejected = false;
    for component in std::path::Path::new(&path).components() {
        match component {
            std::path::Component::Normal(part) => full.push(part),
            _ => {
                rejected = true;
                break;
            }
        }
    }
    if rejected || !full.is_file() {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"ok": false, "error": "not found"})),
        )
            .into_response();
    }
    let content_type = match full.extension().and_then(|ext| ext.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "application/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        _ => "application/octet-stream",
    };
    match tokio::fs::read(&full).await {
        Ok(bytes) => ([(axum::http::header::CONTENT_TYPE, content_type)], bytes).into_response(),
        Err(_) => (
            StatusCode::NOT_FOUND,
            Json(json!({"ok": false, "error": "not found"})),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::router;
    use ghost_core::GhostConfig;
    use tower::ServiceExt;

    fn test_state() -> (tempfile::TempDir, AppState) {
        let dir = tempfile::tempdir().unwrap();
        let mut config = GhostConfig::from_env();
        config.state_dir = dir.path().to_path_buf();
        // Offline: handler tests must not depend on provider keys.
        let state = AppState::new(config, None).unwrap().with_offline(true);
        (dir, state)
    }

    #[test]
    fn build_kernel_offline_returns_honest_note() {
        // Sync test (no tokio runtime): reqwest blocking clients cannot be
        // constructed inside an async context.
        // No provider credentials are expected in this environment.
        let registry = ProviderRegistry::from_env();
        let (kernel, note) = build_kernel("supervised");
        assert_eq!(kernel.autonomy_level(), "supervised");
        if registry.is_empty() {
            let note = note.expect("offline kernel must carry an honest note");
            assert!(note.contains("offline deterministic backend"));
            let executions = kernel.run("what is 6 * 7").unwrap();
            assert_eq!(executions[0].output, "42");
        } else {
            assert!(note.is_none(), "provider present: no offline note");
        }
    }

    #[tokio::test]
    async fn conversation_honest_deterministic_reply() {
        let (_dir, state) = test_state();
        let (router, _count) = router(state.clone());
        let request = axum::http::Request::builder()
            .method("POST")
            .uri("/api/conversation/message")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(r#"{"message": "what is 6 * 7"}"#))
            .unwrap();
        let response = router.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["ok"], true);
        // The run was journaled.
        let run_id = value["trust_run_id"].as_str().expect("trust_run_id");
        let view = state.trust.get_run(run_id).unwrap().unwrap();
        assert!(view.steps.iter().any(|s| s.step_type == "task_executed"));
        if value.get("note").is_some() {
            // Offline deterministic backend fulfilled the arithmetic.
            assert_eq!(value["reply"], "42");
        }
    }

    #[tokio::test]
    async fn conversation_envelope_has_exactly_one_reply_field() {
        let (_dir, state) = test_state();
        let (router, _count) = router(state);
        let request = axum::http::Request::builder()
            .method("POST")
            .uri("/api/conversation/message")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(r#"{"message": "what is 6 * 7"}"#))
            .unwrap();
        let response = router.oneshot(request).await.unwrap();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        let obj = value.as_object().expect("envelope is an object");
        let reply_key_count = obj.keys().filter(|k| k.as_str() == "reply").count();
        assert_eq!(reply_key_count, 1, "envelope: {value}");
        // No nested duplicate replies either (bug #2).
        let text = serde_json::to_string(&value).unwrap();
        assert_eq!(text.matches("\"reply\"").count(), 1, "text: {text}");
    }

    #[tokio::test]
    async fn conversation_empty_message_rejected() {
        let (_dir, state) = test_state();
        let (router, _count) = router(state);
        let request = axum::http::Request::builder()
            .method("POST")
            .uri("/api/conversation/message")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(r#"{"message": "   "}"#))
            .unwrap();
        let response = router.oneshot(request).await.unwrap();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["ok"], false);
    }

    #[tokio::test]
    async fn conversation_unfulfillable_envelope_is_honest_failure() {
        // The envelope's top-level ok must be false when no execution
        // fulfilled the objective — a hardcoded ok:true here would be the
        // PR #116 class of false-success bug.
        let (_dir, state) = test_state();
        let (router, _count) = router(state);
        let request = axum::http::Request::builder()
            .method("POST")
            .uri("/api/conversation/message")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(
                r#"{"message": "write a poem about the ocean"}"#,
            ))
            .unwrap();
        let response = router.oneshot(request).await.unwrap();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["ok"], false);
        let text = serde_json::to_string(&value).unwrap();
        assert!(
            text.contains("deterministic_unfulfilled"),
            "envelope must carry the machine-readable reason: {text}"
        );
    }

    #[tokio::test]
    async fn replay_endpoint_on_journaled_run() {
        let (_dir, state) = test_state();
        // Seed a run to replay.
        let run = state
            .trust
            .create_run("what is 6 * 7", "conversation", "ghost-console", None)
            .unwrap();
        let (router, _count) = router(state);
        let uri = format!("/api/trust/replay/{}", run.run_id);
        let request = axum::http::Request::builder()
            .method("POST")
            .uri(&uri)
            .body(axum::body::Body::empty())
            .unwrap();
        let response = router.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["ok"], true);
        assert_eq!(value["replayed_from"], run.run_id);
        assert!(value["replay_run_id"].as_str().is_some());
        assert!(value["comparison"].is_object());
    }

    #[tokio::test]
    async fn replay_endpoint_unknown_run_404() {
        let (_dir, state) = test_state();
        let (router, _count) = router(state);
        let request = axum::http::Request::builder()
            .method("POST")
            .uri("/api/trust/replay/nope")
            .body(axum::body::Body::empty())
            .unwrap();
        let response = router.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}
