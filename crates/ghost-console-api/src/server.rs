//! Console server: binding, auth, and route assembly.
//!
//! Security posture (GAP [security/C1]):
//!
//! - `serve()` FAILS CLOSED: binding a non-loopback address without a
//!   console token is refused with a config error. The console can only
//!   listen on the network when a bearer token is configured.
//! - Every `/api/*` route passes through the bearer-token middleware.
//!   When no token is configured the API is open — a deliberate
//!   loopback-only dev convenience, only reachable because `serve()`
//!   keeps the socket on localhost in that mode.
//! - A whitespace-only token counts as *no* token (it used to satisfy
//!   the fail-closed check while being trivially guessable).
//! - Tokens are compared in constant time.
//! - Open (tokenless) mode also rejects browser requests that are not
//!   addressed to and coming from loopback (GAP [security/m2]): a
//!   non-loopback `Host` header (DNS rebinding) or a non-loopback
//!   `Origin` (cross-site drive-by POST from any web page) gets 403.
//!   Requests without these headers (curl, the CLI, tests) are allowed.

use axum::{
    extract::State,
    http::{header, Request, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Extension, Json, Router,
};
use serde_json::json;
use std::net::SocketAddr;
use std::path::PathBuf;

use ghost_core::{GhostConfig, GhostError, Result};

use crate::routes;
use crate::state::AppState;

/// True for loopback hosts: `127.0.0.1`, `::1`, `localhost`
/// (case-insensitive, optional `[]` brackets stripped).
pub fn is_loopback_host(host: &str) -> bool {
    let trimmed = host.trim().to_ascii_lowercase();
    let bare = trimmed
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(&trimmed);
    matches!(bare, "127.0.0.1" | "::1" | "localhost")
}

/// Normalize a configured console token: trimmed, and blank means none.
pub fn normalize_token(token: Option<String>) -> Option<String> {
    token
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
}

/// Constant-time byte comparison (length is not secret).
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Host part of a `Host` header value (`host`, `host:port`, `[v6]:port`).
fn host_without_port(value: &str) -> &str {
    let value = value.trim();
    if let Some(rest) = value.strip_prefix('[') {
        // [::1]:8766 -> ::1
        return rest.split(']').next().unwrap_or("");
    }
    match value.rsplit_once(':') {
        // Exactly one colon: host:port. More than one: bare IPv6.
        Some((host, _port)) if !host.contains(':') => host,
        _ => value,
    }
}

/// True when a `Host` header names a loopback host (any port).
pub fn is_loopback_host_header(value: &str) -> bool {
    is_loopback_host(host_without_port(value))
}

/// True when an `Origin` header is `http(s)://<loopback>[:port]`.
/// The opaque origin `null` (sandboxed iframes, `file:`) is NOT loopback.
pub fn is_loopback_origin(value: &str) -> bool {
    let value = value.trim();
    let rest = match value
        .strip_prefix("http://")
        .or_else(|| value.strip_prefix("https://"))
    {
        Some(rest) => rest,
        None => return false,
    };
    let authority = rest.split('/').next().unwrap_or("");
    !authority.contains('@') && is_loopback_host_header(authority)
}

/// Open-mode browser guard: reject DNS-rebinding and cross-site requests.
fn open_mode_request_allowed(headers: &axum::http::HeaderMap) -> bool {
    let header_ok = |name: header::HeaderName, check: fn(&str) -> bool| match headers.get(name) {
        None => true,
        Some(value) => value.to_str().map(check).unwrap_or(false),
    };
    header_ok(header::HOST, is_loopback_host_header)
        && header_ok(header::ORIGIN, is_loopback_origin)
}

/// Bearer-token gate for all `/api/*` routes.
///
/// When `state.token` is `Some` and non-empty, requests must carry
/// `Authorization: Bearer <token>`; anything else gets
/// `401 {"ok": false, "error": "unauthorized"}`. When no token is
/// configured the API is open (loopback dev convenience; see module docs).
async fn auth_middleware(
    State(state): State<AppState>,
    request: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let token = state.token.as_deref().map(str::trim).unwrap_or("");
    if token.is_empty() {
        // Open dev mode; serve() keeps this on loopback only. Still refuse
        // requests a hostile web page could make through the browser.
        if !open_mode_request_allowed(request.headers()) {
            return (
                StatusCode::FORBIDDEN,
                Json(json!({"ok": false, "error": "forbidden: non-loopback host or origin"})),
            )
                .into_response();
        }
        return next.run(request).await;
    }
    let authorized = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(|presented| constant_time_eq(presented.trim().as_bytes(), token.as_bytes()))
        .unwrap_or(false);
    if authorized {
        next.run(request).await
    } else {
        (
            StatusCode::UNAUTHORIZED,
            Json(json!({"ok": false, "error": "unauthorized"})),
        )
            .into_response()
    }
}

/// Build the console router. Returns the router and the API route count.
///
/// Static files are mounted on `/*path` with an absolute path derived from
/// `CARGO_MANIFEST_DIR` (the `./static` directory copied next to this
/// crate). Note: axum's `nest_service` rejects wildcard paths, so the
/// static mount uses a `/*path` route to the [`routes::static_file`]
/// handler instead — same behavior (all unmatched paths resolve under
/// `./static`, including `/index.html`).
pub fn router(state: AppState) -> (Router, usize) {
    let api_routes: Vec<(&str, axum::routing::MethodRouter<AppState>)> = vec![
        ("/console/status", get(routes::status_handler)),
        ("/conversation/message", post(routes::conversation_message)),
        ("/trust/runs", get(routes::trust_list_runs)),
        ("/trust/runs/:id", get(routes::trust_get_run)),
        ("/trust/replay/:id", post(routes::trust_replay)),
        (
            "/standing-orders",
            get(routes::standing_orders_list).post(routes::standing_orders_create),
        ),
        (
            "/standing-orders/:id/enable",
            post(routes::standing_orders_enable),
        ),
        (
            "/standing-orders/:id/disable",
            post(routes::standing_orders_disable),
        ),
        (
            "/standing-orders/:id/run",
            post(routes::standing_orders_run),
        ),
    ];
    // API routes + the root index route.
    let route_count = api_routes.len() + 1;
    let mut api = Router::new();
    for (path, method_router) in api_routes {
        api = api.route(path, method_router);
    }
    let api = api.layer(middleware::from_fn_with_state(
        state.clone(),
        auth_middleware,
    ));

    let app = Router::new()
        .nest("/api", api)
        .route("/", get(routes::index_html))
        .route("/*path", get(routes::static_file))
        .layer(Extension(route_count))
        .with_state(state);
    (app, route_count)
}

/// Normalize a bind host for `SocketAddr` parsing: `localhost` becomes
/// `127.0.0.1` (SocketAddr parsing does no DNS), and bare IPv6 literals
/// get brackets.
fn normalize_bind_host(host: &str) -> String {
    let trimmed = host.trim();
    if trimmed.eq_ignore_ascii_case("localhost") {
        return "127.0.0.1".to_string();
    }
    if trimmed.contains(':') && !trimmed.starts_with('[') {
        return format!("[{trimmed}]");
    }
    trimmed.to_string()
}

/// Parse a bind address, mapping failures to a config error.
fn bind_addr(host: &str, port: u16) -> Result<SocketAddr> {
    let normalized = normalize_bind_host(host);
    format!("{normalized}:{port}")
        .parse()
        .map_err(|err| GhostError::Config(format!("invalid bind address {host}:{port}: {err}")))
}

/// Serve the console API.
///
/// FAIL-CLOSED (GAP [security/C1]): when `host` is not a loopback address
/// and no `token` is provided, this returns an error WITHOUT binding.
pub async fn serve(host: &str, port: u16, token: Option<String>, state_dir: PathBuf) -> Result<()> {
    let token = normalize_token(token);
    if !is_loopback_host(host) && token.is_none() {
        return Err(GhostError::Config(
            "refusing to bind non-loopback address without a console token (set GHOST_CONSOLE_TOKEN or --token)".to_string(),
        ));
    }
    let mut config = GhostConfig::from_env();
    config.state_dir = state_dir;
    let state = AppState::new(config, token)?;
    let (app, _route_count) = router(state);
    let addr = bind_addr(host, port)?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ghost_core::GhostConfig;
    use serde_json::Value;
    use tower::ServiceExt;

    fn test_state_with_token(token: Option<&str>) -> (tempfile::TempDir, AppState) {
        let dir = tempfile::tempdir().unwrap();
        let mut config = GhostConfig::from_env();
        config.state_dir = dir.path().to_path_buf();
        let state = AppState::new(config, token.map(str::to_string)).unwrap();
        (dir, state)
    }

    #[test]
    fn loopback_hosts_recognized() {
        assert!(is_loopback_host("127.0.0.1"));
        assert!(is_loopback_host("::1"));
        assert!(is_loopback_host("[::1]"));
        assert!(is_loopback_host("localhost"));
        assert!(is_loopback_host("LOCALHOST"));
        assert!(is_loopback_host(" 127.0.0.1 "));
        assert!(!is_loopback_host("0.0.0.0"));
        assert!(!is_loopback_host("192.168.1.10"));
        assert!(!is_loopback_host("example.com"));
        assert!(!is_loopback_host(""));
    }

    #[tokio::test]
    async fn non_loopback_bind_without_token_refuses() {
        let dir = tempfile::tempdir().unwrap();
        let err = serve("0.0.0.0", 8766, None, dir.path().to_path_buf())
            .await
            .expect_err("must refuse");
        assert!(
            matches!(err, GhostError::Config(_)),
            "expected Config, got: {err}"
        );
        assert!(err.to_string().contains("refusing to bind non-loopback"));
    }

    #[test]
    fn bind_addr_normalizes_loopback_hosts() {
        let addr = bind_addr("127.0.0.1", 8766).unwrap();
        assert_eq!(addr.to_string(), "127.0.0.1:8766");
        let addr = bind_addr("localhost", 8766).unwrap();
        assert_eq!(addr.to_string(), "127.0.0.1:8766");
        let addr = bind_addr("::1", 8766).unwrap();
        assert_eq!(addr.to_string(), "[::1]:8766");
        let addr = bind_addr("[::1]", 8766).unwrap();
        assert_eq!(addr.to_string(), "[::1]:8766");
        let err = bind_addr("not a host!!", 1).expect_err("must fail");
        assert!(err.to_string().contains("invalid bind address"));
    }

    #[tokio::test]
    async fn loopback_serve_with_token_binds() {
        let dir = tempfile::tempdir().unwrap();
        let state_dir = dir.path().to_path_buf();
        let handle = tokio::spawn(async move {
            serve("localhost", 0, Some("test-token".to_string()), state_dir).await
        });
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        // Still serving (port 0 = OS-assigned): the bind succeeded.
        assert!(
            !handle.is_finished(),
            "serve should still be running after a successful bind"
        );
        handle.abort();
    }

    #[tokio::test]
    async fn auth_rejects_missing_bearer_token() {
        let (_dir, state) = test_state_with_token(Some("secret-token"));
        let (router, _count) = router(state);
        let request = axum::http::Request::builder()
            .method("GET")
            .uri("/api/console/status")
            .body(axum::body::Body::empty())
            .unwrap();
        let response = router.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value, json!({"ok": false, "error": "unauthorized"}));
    }

    #[tokio::test]
    async fn auth_accepts_correct_bearer_token() {
        let (_dir, state) = test_state_with_token(Some("secret-token"));
        let (router, _count) = router(state);
        let request = axum::http::Request::builder()
            .method("GET")
            .uri("/api/console/status")
            .header("Authorization", "Bearer secret-token")
            .body(axum::body::Body::empty())
            .unwrap();
        let response = router.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn auth_open_when_no_token_configured() {
        let (_dir, state) = test_state_with_token(None);
        let (router, _count) = router(state);
        let request = axum::http::Request::builder()
            .method("GET")
            .uri("/api/console/status")
            .body(axum::body::Body::empty())
            .unwrap();
        let response = router.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn auth_rejects_wrong_bearer_token() {
        // Kills mutants that weaken the token comparison (e.g. `==` -> `!=`
        // inversion or always-true). A wrong token must get 401.
        let (_dir, state) = test_state_with_token(Some("secret-token"));
        let (router, _count) = router(state);
        let request = axum::http::Request::builder()
            .method("GET")
            .uri("/api/console/status")
            .header("Authorization", "Bearer wrong-token")
            .body(axum::body::Body::empty())
            .unwrap();
        let response = router.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value, json!({"ok": false, "error": "unauthorized"}));
    }

    #[tokio::test]
    async fn auth_rejects_malformed_bearer_token() {
        // No "Bearer " prefix -> 401. Kills mutants that skip the prefix check.
        let (_dir, state) = test_state_with_token(Some("secret-token"));
        let (router, _count) = router(state);
        let request = axum::http::Request::builder()
            .method("GET")
            .uri("/api/console/status")
            .header("Authorization", "secret-token")
            .body(axum::body::Body::empty())
            .unwrap();
        let response = router.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn loopback_serve_without_token_binds() {
        // Kills the `&&` -> `||` mutant in serve()'s fail-closed guard.
        // With `||`, a loopback bind with no token would incorrectly refuse.
        // Loopback + no token must bind successfully (open dev mode).
        let dir = tempfile::tempdir().unwrap();
        let state_dir = dir.path().to_path_buf();
        let handle = tokio::spawn(async move { serve("127.0.0.1", 0, None, state_dir).await });
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        assert!(
            !handle.is_finished(),
            "serve on loopback without token should bind, not refuse"
        );
        handle.abort();
    }

    #[tokio::test]
    async fn non_loopback_bind_with_token_binds() {
        // Complement: non-loopback WITH a token must bind (not refuse).
        // Kills mutants that invert the guard condition.
        let dir = tempfile::tempdir().unwrap();
        let state_dir = dir.path().to_path_buf();
        let handle =
            tokio::spawn(
                async move { serve("0.0.0.0", 0, Some("tok".to_string()), state_dir).await },
            );
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        assert!(
            !handle.is_finished(),
            "serve on non-loopback with token should bind"
        );
        handle.abort();
    }

    #[tokio::test]
    async fn status_returns_version_and_route_count() {
        let (_dir, state) = test_state_with_token(None);
        let (router, count) = router(state.clone());
        let request = axum::http::Request::builder()
            .method("GET")
            .uri("/api/console/status")
            .body(axum::body::Body::empty())
            .unwrap();
        let response = router.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["ok"], true);
        assert_eq!(value["version"], state.version);
        assert_eq!(value["routes"], count);
        assert_eq!(value["mode"], "open");
    }

    #[tokio::test]
    async fn standing_orders_full_lifecycle_over_http() {
        let (_dir, state) = test_state_with_token(None);
        let (router, _count) = router(state);

        async fn post(router: &Router, uri: &str, body: &str) -> (StatusCode, Value) {
            let request = axum::http::Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .body(axum::body::Body::from(body.to_string()))
                .unwrap();
            let response = router.clone().oneshot(request).await.unwrap();
            let status = response.status();
            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            (status, serde_json::from_slice(&bytes).unwrap())
        }

        async fn get(router: &Router, uri: &str) -> (StatusCode, Value) {
            let request = axum::http::Request::builder()
                .method("GET")
                .uri(uri)
                .body(axum::body::Body::empty())
                .unwrap();
            let response = router.clone().oneshot(request).await.unwrap();
            let status = response.status();
            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            (status, serde_json::from_slice(&bytes).unwrap())
        }

        // Create.
        let (status, value) = post(
            &router,
            "/api/standing-orders",
            r#"{"title": "Daily scan", "objective": "scan for news"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{value}");
        let id = value["order"]["id"].as_str().unwrap().to_string();
        assert_eq!(value["order"]["status"], "disabled");

        // Invalid create -> 400.
        let (status, _) = post(
            &router,
            "/api/standing-orders",
            r#"{"title": "", "objective": "x"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        // List with counts.
        let (status, value) = get(&router, "/api/standing-orders").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(value["counts"]["orders"], 1);
        assert_eq!(value["counts"]["disabled"], 1);

        // Enable.
        let (status, value) =
            post(&router, &format!("/api/standing-orders/{id}/enable"), "{}").await;
        assert_eq!(status, StatusCode::OK, "{value}");
        assert_eq!(value["order"]["status"], "enabled");

        // Run: honest preview, no counter bump (HON-F2).
        let (status, value) = post(&router, &format!("/api/standing-orders/{id}/run"), "{}").await;
        assert_eq!(status, StatusCode::OK, "{value}");
        assert_eq!(value["ok"], true);
        assert_eq!(value["executed"], false);
        assert_eq!(value["preview"], true);
        assert_eq!(value["objective"], "scan for news");
        assert_eq!(value["order"]["run_count"], 0);

        // Disable, then run is refused.
        let (status, _) = post(&router, &format!("/api/standing-orders/{id}/disable"), "{}").await;
        assert_eq!(status, StatusCode::OK);
        let (status, value) = post(&router, &format!("/api/standing-orders/{id}/run"), "{}").await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{value}");

        // Unknown order -> 404.
        let (status, _) = post(&router, "/api/standing-orders/nope/enable", "{}").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn non_loopback_bind_with_blank_token_refuses() {
        let dir = tempfile::tempdir().unwrap();
        let err = serve(
            "0.0.0.0",
            8766,
            Some("   ".to_string()),
            dir.path().to_path_buf(),
        )
        .await
        .expect_err("blank token is no token");
        assert!(err.to_string().contains("refusing to bind non-loopback"));
    }

    #[test]
    fn normalize_token_trims_and_drops_blank() {
        assert_eq!(normalize_token(None), None);
        assert_eq!(normalize_token(Some(String::new())), None);
        assert_eq!(normalize_token(Some(" \t ".to_string())), None);
        assert_eq!(
            normalize_token(Some(" abc ".to_string())),
            Some("abc".to_string())
        );
    }

    #[test]
    fn constant_time_eq_semantics() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn host_header_loopback_detection() {
        assert!(is_loopback_host_header("127.0.0.1"));
        assert!(is_loopback_host_header("127.0.0.1:8766"));
        assert!(is_loopback_host_header("localhost:3000"));
        assert!(is_loopback_host_header("[::1]:8766"));
        assert!(is_loopback_host_header("::1"));
        assert!(!is_loopback_host_header("evil.example:8766"));
        assert!(!is_loopback_host_header("127.0.0.1.evil.example"));
        assert!(!is_loopback_host_header("[::2]:80"));
        assert_eq!(host_without_port("a:1"), "a");
        assert_eq!(host_without_port("[::1]:9"), "::1");
        assert_eq!(host_without_port("::1"), "::1");
        assert_eq!(host_without_port(" a "), "a");
    }

    #[test]
    fn origin_loopback_detection() {
        assert!(is_loopback_origin("http://127.0.0.1:8766"));
        assert!(is_loopback_origin("http://localhost"));
        assert!(is_loopback_origin("https://[::1]:8443"));
        assert!(!is_loopback_origin("null"));
        assert!(!is_loopback_origin("file://"));
        assert!(!is_loopback_origin("https://evil.example"));
        assert!(!is_loopback_origin("http://localhost.evil.example"));
        assert!(!is_loopback_origin("http://127.0.0.1@evil.example"));
    }

    async fn status_with_headers(token: Option<&str>, headers: &[(&str, &str)]) -> StatusCode {
        let (_dir, state) = test_state_with_token(token);
        let (router, _count) = router(state);
        let mut builder = axum::http::Request::builder()
            .method("POST")
            .uri("/api/standing-orders/nope/run");
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let response = router
            .oneshot(builder.body(axum::body::Body::empty()).unwrap())
            .await
            .unwrap();
        response.status()
    }

    #[tokio::test]
    async fn open_mode_rejects_cross_site_and_rebinding_requests_m2() {
        // Drive-by POST from a web page.
        assert_eq!(
            status_with_headers(None, &[("origin", "https://evil.example")]).await,
            StatusCode::FORBIDDEN
        );
        // DNS rebinding: request reaches loopback with the attacker's Host.
        assert_eq!(
            status_with_headers(None, &[("host", "evil.example:8766")]).await,
            StatusCode::FORBIDDEN
        );
        // Opaque origin.
        assert_eq!(
            status_with_headers(None, &[("origin", "null")]).await,
            StatusCode::FORBIDDEN
        );
    }

    #[tokio::test]
    async fn open_mode_allows_same_origin_ui_and_headerless_clients() {
        // The console UI / Electron shell: loopback Host + Origin.
        assert_eq!(
            status_with_headers(
                None,
                &[
                    ("host", "127.0.0.1:8766"),
                    ("origin", "http://127.0.0.1:8766")
                ]
            )
            .await,
            StatusCode::NOT_FOUND // reached the handler: unknown order id
        );
        // curl / CLI: no Origin.
        assert_eq!(
            status_with_headers(None, &[("host", "localhost:8766")]).await,
            StatusCode::NOT_FOUND
        );
        assert_eq!(status_with_headers(None, &[]).await, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn token_mode_requires_exact_token() {
        assert_eq!(
            status_with_headers(Some("s3cret"), &[("authorization", "Bearer s3cre")]).await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            status_with_headers(Some("s3cret"), &[("authorization", "Bearer s3cret")]).await,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            status_with_headers(Some("s3cret"), &[("authorization", "s3cret")]).await,
            StatusCode::UNAUTHORIZED
        );
    }
}
