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
use ghost_providers::{CostLedger, MeteredProvider, ModelProvider, ProviderRegistry};
use ghost_trust::{RecordStepArgs, ReplayExecution};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

use crate::state::AppState;

/// Build the pilot kernel for conversation/replay work.
///
/// Uses the first available model provider from the environment; when none
/// is configured, falls back to the offline deterministic backend and
/// returns an honest `note` saying so (bug #5: never claim a model ran).
///
/// Every model call is metered (GAP [durability/H1]): the provider is
/// wrapped in a [`MeteredProvider`] backed by the durable cost ledger at
/// `<state_dir>/cost_ledger.jsonl`, so budget caps are enforced before a
/// request is sent and every call is journaled. If the ledger cannot be
/// opened, model providers are disabled (fail closed) and the returned
/// note says why; the deterministic backend still runs.
///
/// NOTE: `ProviderRegistry::from_env()` eagerly constructs reqwest
/// *blocking* clients, which panic when built inside a tokio async
/// context. Call [`build_kernel_async`] from async code instead.
pub fn build_kernel(
    autonomy_level: &str,
    state_dir: &std::path::Path,
) -> (ChimeraPilotKernel, Option<String>) {
    let registry = ProviderRegistry::from_env();
    let raw = registry
        .available_names()
        .first()
        .and_then(|name| registry.get(name));
    build_kernel_with(autonomy_level, state_dir, raw)
}

/// [`build_kernel`] with an explicit (un-metered) provider choice.
pub(crate) fn build_kernel_with(
    autonomy_level: &str,
    state_dir: &std::path::Path,
    raw: Option<Arc<dyn ModelProvider>>,
) -> (ChimeraPilotKernel, Option<String>) {
    let (provider, ledger_note) = match raw {
        None => (None, None),
        Some(raw) => match open_ledger(state_dir) {
            Ok(ledger) => {
                let metered: Arc<dyn ModelProvider> =
                    Arc::new(MeteredProvider::from_env(raw, ledger));
                (Some(metered), None)
            }
            Err(err) => (
                None,
                Some(format!(
                    "Model providers disabled: the cost ledger could not be opened ({err}). {OFFLINE_NOTE}"
                )),
            ),
        },
    };
    let options = KernelOptions {
        include_deterministic_backend: true,
        deterministic_fulfill: true,
        include_model_provider_backend: provider.is_some(),
        autonomy_level: autonomy_level.to_string(),
    };
    let kernel = ChimeraPilotKernel::default_with(options, provider.clone());
    let note = match (&provider, ledger_note) {
        (_, Some(ledger_note)) => Some(ledger_note),
        (Some(_), None) => None,
        (None, None) => Some(OFFLINE_NOTE.to_string()),
    };
    (kernel, note)
}

/// Open the durable cost ledger under `state_dir` (creating the dir).
pub fn open_ledger(state_dir: &std::path::Path) -> Result<Arc<CostLedger>, GhostError> {
    std::fs::create_dir_all(state_dir)?;
    Ok(Arc::new(CostLedger::open(state_dir)?))
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
        build_kernel_async(&state.config.autonomy_level, &state.config.state_dir).await
    }
}

/// Async wrapper around [`build_kernel`]: constructs the kernel on a
/// blocking thread, because reqwest blocking clients cannot be built
/// inside a tokio async context (they panic dropping their internal
/// runtime there).
pub async fn build_kernel_async(
    autonomy_level: &str,
    state_dir: &std::path::Path,
) -> (ChimeraPilotKernel, Option<String>) {
    let autonomy_level = autonomy_level.to_string();
    let state_dir = state_dir.to_path_buf();
    tokio::task::spawn_blocking(move || build_kernel(&autonomy_level, &state_dir))
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

/// The console UI, embedded into the binary at compile time.
///
/// Previously the files were read at runtime from
/// `env!("CARGO_MANIFEST_DIR")/static`, an absolute path on the *build*
/// machine: every released binary (CLI zip, Electron sidecar) served 404s
/// for the console UI on users' machines. Embedding makes the binary
/// self-contained.
const STATIC_FILES: &[(&str, &[u8])] = &[
    ("index.html", include_bytes!("../static/index.html")),
    ("app.js", include_bytes!("../static/app.js")),
    (
        "api_transport.js",
        include_bytes!("../static/api_transport.js"),
    ),
    ("styles.css", include_bytes!("../static/styles.css")),
    (
        "nav_groups.json",
        include_bytes!("../static/nav_groups.json"),
    ),
];

/// Look up an embedded static file by its request path (no leading `/`).
/// Only exact names match, so traversal (`../`, absolute paths, encoded
/// separators) can never reach the filesystem.
pub fn embedded_file(path: &str) -> Option<&'static [u8]> {
    STATIC_FILES
        .iter()
        .find(|(name, _)| *name == path)
        .map(|(_, bytes)| *bytes)
}

/// Content type by file extension.
fn content_type_for(path: &str) -> &'static str {
    match path.rsplit_once('.').map(|(_, ext)| ext) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "application/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        _ => "application/octet-stream",
    }
}

fn not_found(message: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({"ok": false, "error": message})),
    )
        .into_response()
}

/// Serve the console `index.html`.
pub async fn index_html() -> Response {
    match embedded_file("index.html") {
        Some(bytes) => (
            [(
                axum::http::header::CONTENT_TYPE,
                content_type_for("index.html"),
            )],
            bytes,
        )
            .into_response(),
        None => not_found("index.html not found"),
    }
}

/// Serve an embedded console asset; unknown paths 404.
pub async fn static_file(Path(path): Path<String>) -> Response {
    match embedded_file(&path) {
        Some(bytes) => (
            [(axum::http::header::CONTENT_TYPE, content_type_for(&path))],
            bytes,
        )
            .into_response(),
        None => not_found("not found"),
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
        let dir = tempfile::tempdir().unwrap();
        let (kernel, note) = build_kernel("supervised", dir.path());
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

    /// Fake provider for kernel-wiring tests (never touches the network).
    struct EchoProvider;

    impl ModelProvider for EchoProvider {
        fn name(&self) -> &str {
            "echo"
        }
        fn chat(
            &self,
            _system: &str,
            user: &str,
        ) -> Result<String, ghost_providers::ProviderError> {
            Ok(format!("echo: {user}"))
        }
        fn validate_config(&self) -> Vec<String> {
            Vec::new()
        }
        fn is_available(&self) -> bool {
            true
        }
    }

    #[test]
    fn kernel_meters_every_provider_call_h1() {
        let dir = tempfile::tempdir().unwrap();
        let state_dir = dir.path().join("state");
        let (kernel, note) =
            build_kernel_with("supervised", &state_dir, Some(Arc::new(EchoProvider)));
        assert!(note.is_none(), "provider configured: no offline note");
        let executions = kernel.run("write a poem").unwrap();
        let by_echo = executions
            .iter()
            .find(|e| e.backend_id == "echo")
            .expect("provider backend ran");
        assert!(by_echo.ok);
        // The call was journaled durably by the metered wrapper.
        let ledger = CostLedger::open(&state_dir).unwrap();
        assert_eq!(ledger.calls("echo"), 1);
    }

    #[test]
    fn kernel_refuses_provider_over_budget_h1() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = CostLedger::open(dir.path()).unwrap();
        ledger.set_budget("echo", 0.0).unwrap();
        drop(ledger);
        let (kernel, _note) =
            build_kernel_with("supervised", dir.path(), Some(Arc::new(EchoProvider)));
        let executions = kernel.run("write a poem").unwrap();
        assert!(executions.iter().all(|e| !e.ok), "{executions:?}");
        assert!(
            executions
                .iter()
                .any(|e| e.error.contains("budget exceeded")),
            "{executions:?}"
        );
        assert_eq!(CostLedger::open(dir.path()).unwrap().calls("echo"), 0);
    }

    #[test]
    fn kernel_disables_providers_when_ledger_unavailable_h1() {
        let dir = tempfile::tempdir().unwrap();
        // state_dir is a FILE: the ledger cannot be opened.
        let state_dir = dir.path().join("not-a-dir");
        std::fs::write(&state_dir, b"x").unwrap();
        let (kernel, note) =
            build_kernel_with("supervised", &state_dir, Some(Arc::new(EchoProvider)));
        let note = note.expect("honest note");
        assert!(note.contains("cost ledger could not be opened"), "{note}");
        let executions = kernel.run("what is 6 * 7").unwrap();
        assert!(executions.iter().all(|e| e.backend_id != "echo"));
        assert_eq!(executions[0].output, "42");
    }

    #[test]
    fn kernel_without_provider_is_offline_with_note() {
        let dir = tempfile::tempdir().unwrap();
        let (kernel, note) = build_kernel_with("supervised", dir.path(), None);
        assert_eq!(note.as_deref(), Some(OFFLINE_NOTE));
        assert_eq!(kernel.run("what is 6 * 7").unwrap()[0].output, "42");
    }

    #[test]
    fn every_embedded_asset_is_present_and_non_empty() {
        for name in [
            "index.html",
            "app.js",
            "api_transport.js",
            "styles.css",
            "nav_groups.json",
        ] {
            let bytes = embedded_file(name).unwrap_or_else(|| panic!("{name} embedded"));
            assert!(!bytes.is_empty(), "{name} empty");
        }
        assert!(embedded_file("../Cargo.toml").is_none());
        assert!(embedded_file("/etc/passwd").is_none());
        assert!(embedded_file("").is_none());
    }

    #[test]
    fn content_types_by_extension() {
        assert_eq!(content_type_for("a.html"), "text/html; charset=utf-8");
        assert_eq!(
            content_type_for("a.js"),
            "application/javascript; charset=utf-8"
        );
        assert_eq!(content_type_for("a.css"), "text/css; charset=utf-8");
        assert_eq!(content_type_for("a.json"), "application/json");
        assert_eq!(content_type_for("a.svg"), "image/svg+xml");
        assert_eq!(content_type_for("a.png"), "image/png");
        assert_eq!(content_type_for("a.ico"), "image/x-icon");
        assert_eq!(content_type_for("noext"), "application/octet-stream");
    }

    async fn get_raw(uri: &str) -> (StatusCode, String, Vec<u8>) {
        let (_dir, state) = test_state();
        let (router, _count) = router(state);
        let request = axum::http::Request::builder()
            .uri(uri)
            .body(axum::body::Body::empty())
            .unwrap();
        let response = router.oneshot(request).await.unwrap();
        let status = response.status();
        let ctype = response
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .map(|v| v.to_str().unwrap().to_string())
            .unwrap_or_default();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec();
        (status, ctype, body)
    }

    #[tokio::test]
    async fn ui_is_served_from_the_binary() {
        let (status, ctype, body) = get_raw("/").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(ctype, "text/html; charset=utf-8");
        assert_eq!(body, embedded_file("index.html").unwrap());
        let (status, ctype, body) = get_raw("/app.js").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(ctype, "application/javascript; charset=utf-8");
        assert_eq!(body, embedded_file("app.js").unwrap());
        let (status, _, _) = get_raw("/nope.js").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _, _) = get_raw("/..%2FCargo.toml").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
}
