# GHOST-Chimera Rust Rebuild — Phase 0 Specification

**Status:** Phase 0 (spec + architecture). No implementation code.
**Ordered by:** Inan (Fernando Garza), 2026-10-11
**Source of truth for behavior:** `~/workspace/ghost-chimera` (Python, ~78k LOC, 275 files, 207 test files)
**Correct-behavior reference:** Python PRs #116 (`fix/ask-deterministic-honesty`) and #117 (`fix/readme-truth-honest`)

## Goal

Rebuild GHOST-Chimera as production-ready real software:

1. A Rust core workspace (library crates + CLI).
2. A desktop Ghost Console application with a real installer (not browser-served).
3. Every known bug fixed during the rebuild (see Section 6).
4. Engineering bar: no stubs, no TODOs, `clippy` + `rustfmt` clean, mutation testing in CI.

Non-goals for the rebuild: new product features, cloud/SaaS expansion, benchmarking.

---

## 1. Workspace layout

Cargo workspace root: `~/workspace/ghost-chimera-rust/` (this directory).

```
ghost-chimera-rust/
  Cargo.toml                 # workspace
  SPEC.md                    # this document
  crates/
    ghost-core/              # config, errors, redaction, ids, time
    ghost-trust/             # TrustRuntimeStore: journals, approvals, replay
    ghost-stealth/           # StealthLoop: events, world state, hosts, learning
    ghost-pilot/             # ChimeraPilotKernel: tasks, backends, execution
    ghost-providers/         # ModelProvider trait + OpenAI-compatible client
    ghost-tools/             # Tool layer: fs, shell, web, browser workspace
    ghost-memory/            # MemoryStore (SQLite), consolidation, RAG ingest
    ghost-safety/            # policy, approvals, SSRF guard, rate limiting
    ghost-console-api/       # axum HTTP API (the 240 routes), conversation loop
    ghost-cli/               # clap CLI: ask, trust, standing-orders, console, ...
    ghost-desktop/           # desktop shell (Tauri or Electron, see Section 3)
```

### ghost-core

Responsibility: shared primitives every other crate depends on. No domain logic.

Key types:

- `GhostConfig` — mirrors `GhostChimeraConfig` (`config.py`): `state_dir`, `memory_db`, `audit_file`, loaded from env (`GHOSTCHIMERA_STATE_DIR`, default `~/.ghostchimera`) + config file. Uses the `directories` crate for OS-correct paths (Section 7).
- `GhostError` — `thiserror` enum: `Io`, `Config`, `PolicyDenied`, `ApprovalRequired`, `BackendUnavailable`, `Unfulfilled(String)`, `NotFound`, `Serialization`.
- `redact()` — mirrors `redaction.py`: redacts secret-looking values before journaling/logging.
- `new_id(prefix)` — stable id generation (mirrors `_stable_id` in `trust_runtime.py`: hash of inputs + timestamp, hex, truncated).
- `Utc` helpers — `chrono` based, replaces `_now()`.

Public API: `GhostConfig::from_env()`, `GhostConfig::ensure_state_dirs()`, `redact_value(&serde_json::Value) -> Value`.

### ghost-trust

Responsibility: the Trust Runtime. Every run journaled, approvals, eval baselines, real replay.

Key types (mirroring `trust_runtime.py`):

- `TrustRuntimeStore { state_dir: PathBuf }` — methods: `create_run`, `record_step`, `record_tool_call`, `create_approval`, `resolve_approval`, `resume_run`, `list_runs`, `get_run`, `pending_approvals`, `promote_run_to_eval_case`, `list_eval_cases`, `verify_run_integrity`, `simulate_replay`, `replay_run`, `export_trace`, `trust_status`, `eval_baseline`, `eval_compare`, `save_session`/`get_session`/`list_sessions`/`delete_session` (durable agent sessions).
- `DurableRun`, `RunStepRecord`, `ApprovalCheckpoint`, `ToolCallRecord`, `ToolTrustEnvelope`, `RunResumeToken`, `TrustEvalCase` — serde structs, field-for-field compatible with the Python `to_dict()` shapes (Section 4).
- `replay_run(run_id, executor)` — real re-execution (bug #6). Creates a linked `source="replay"` run, executes via the provided executor or the deterministic kernel, records `replay_execution` + `replay_comparison` steps, returns outcome comparison.

Public API: `TrustRuntimeStore::new(state_dir)`, all methods above. Thread-safe via `Arc<Mutex<..>>` per store file (mirrors `_session_locks`).

### ghost-stealth

Responsibility: the ambient Stealth Loop — EVENT → UNDERSTAND → UPDATE STATE → RECALL EXPERIENCE → MATCH WORKFLOW → PREDICT → DECIDE → PREPARE/INJECT/ACT → OBSERVE OUTCOME → LEARN.

Key types (mirroring `stealth/`):

- `StealthLoop` — `emit(event)`, `inject(intervention_id, host)`, `observe_outcome(id, outcome)`, `get_standing_context()`, `sync_workflows()`, `last_result()`.
- `Event`, `EventBus` (tokio mpsc/broadcast), `WorldState`, `ExperienceGraph`, `WorkflowLearner`, `PredictionEngine`, `StealthEvaluator`, `ContextFabric`, `BackgroundRuntime` (tokio tasks).
- `HostAdapter` trait + `AdapterRegistry`: `ClaudeCodeAdapter`, `OpenClawAdapter`, `OpenCodeAdapter`, `CodexAdapter`, `GeminiAdapter`, `HermesAdapter`. `install()` returns a real config payload (bug #1) — never `{"installed": false}`.
- `ApprovalStore` (stealth/approvals.py), `Intent` classifier, `Intervention`/`InterventionOutcome`.

Public API: `StealthLoop::new(config)`, `HostAdapter` trait with `detect() -> bool`, `install() -> InstallPayload`, `assemble()`, `after_turn()`.

### ghost-pilot

Responsibility: objective → task compilation → backend selection → execution.

Key types (mirroring `chimera_pilot/`):

- `ChimeraPilotKernel` — `ChimeraPilotKernel::default(opts)`, `compile(objective) -> Vec<TaskSpec>`, `execute_task(task) -> PilotExecution`, `run(objective) -> Vec<PilotExecution>`, `run_dag(...)`.
- `TaskSpec { id, kind: TaskKind, objective, ... }`, `TaskKind::{Reasoning, ToolCall, RagQuery, ...}`.
- `Backend` trait: `id()`, `probe() -> BackendHealth`, `can_run(&TaskSpec) -> bool`, `estimate(&TaskSpec) -> BackendHealth`, `execute(&TaskSpec) -> ExecutionResult`.
- `BackendRegistry`, `BackendHealth`, `BackendCapabilities`.
- `DeterministicBackend { fulfill: bool }` — bug #7 lives here: when `fulfill=true`, `_try_fulfill` handles file-listing (real `std::fs::read_dir`) and arithmetic (safe expression evaluator, NOT eval); otherwise returns `ok=false` with `deterministic_unfulfilled: ...` machine-readable error. Never false success.
- `PilotExecution { task, result, ok(), to_json() }` — `ok` is `result.ok && verification_error.is_none()`.

Public API: `ChimeraPilotKernel::default(KernelOptions)`, `KernelOptions { include_deterministic_backend, deterministic_fulfill, include_model_provider_backend, autonomy_level, ... }`.

### ghost-providers

Responsibility: model provider integrations.

- `ModelProvider` trait (mirrors `BaseProvider`): `fn name(&self) -> &str`, `fn chat(&self, system: &str, user: &str) -> Result<String, ProviderError>`, `fn validate_config(&self) -> Vec<String>`, `fn is_available(&self) -> bool`.
- `OpenAiCompatibleProvider { name, base_url, api_key_env, model }` — one `reqwest`-based client covering ~24 of the 28 classes in `openai_compatible_providers.py` (Groq, xAI, Mistral, DeepSeek, Together, OpenRouter, Perplexity, Fireworks, Cerebras, AI21, HuggingFace, Nvidia, Nebius, Moonshot, DeepInfra, Qwen, Volcengine, StepFun, GLM, Venice, Vultr, Cloudflare, Gemini-OAI, OpenAI itself).
- Individual structs for the non-conformant ones: `AnthropicProvider`, `OllamaProvider`, `LmStudioProvider`, `CohereProvider`, `PollinationsProvider`, `GeminiProvider`, `LlamaCppProvider`, `MiniMindProvider`, CLI-bridge providers (`CodexCliProvider`, `OpenCodeCliProvider`, `ClaudeAgentProvider`).
- `FreeRouter` (mirrors `free_router.py` incl. the quota race fix from PR #109: reservation→usage must be one atomic state transition under a single lock), `CostMonitor`, `ProviderAuth`.

Public API: `ModelProvider` trait, `ProviderRegistry::from_env()` (reads `NEBIUS_API_KEY` etc.), `FreeRouter`.

### ghost-tools

Responsibility: tool implementations the pilot can call (mirrors `tool_layer/`).

- `FileSystemTool` (sandboxed to workspace root), `ShellTool` (allowlisted, approval-gated), `WebSearchTool`, `BrowserWorkspace`.
- Every tool call emits a `ToolTrustEnvelope { risk_level }` into the trust journal.

### ghost-memory

Responsibility: durable memory (mirrors `memory_layer/` + `personalization/`).

- `MemoryStore` — SQLite (rusqlite) at `<state_dir>/memory.sqlite3`.
- `DocumentIngester`, `EmailIngester`, `TextSplitter`, `Consolidation`, `TemporalGraph`, `NamespaceStore`.
- Personal context RAG injection for the pilot.

### ghost-safety

Responsibility: policy enforcement (mirrors `safety_layer/`).

- `PolicyEngine`, `ApprovalGate`, `RateLimiter`, `SsrfGuard` (egress allowlist), `SecurityMonitor`, `AuditLog` (HMAC-chained, mirrors verified HMAC-SHA256 audit logging).
- `CapabilityAdmission` (mirrors `capability_admission.py`).

### ghost-console-api

Responsibility: the console backend — axum HTTP server replacing `control_plane/console.py` + `conversation.py` (191 `/api/` path references, 240 total routes reported by the live console).

- `ConsoleServer` — axum `Router`, serves the desktop UI bundle + JSON API.
- `ConversationalLoopController` — session state machine (`listening`/`executing`/`thinking`/`error`), `last_reply` dedup logic (bug #2: the reply box must not re-render the final transcript turn), deterministic fallback (bug #5: when no model provider backend is available, fall back to `DeterministicBackend{fulfill:true}` and attach an honest `note`, never claim success for unfulfillable work).
- `StandingOrderStore` — `create_order` (disabled-by-default), `list_orders`, `enable_order`, `disable_order`, `run_order` (bug #4: CLI parity comes from `ghost-cli` calling the same store, not a separate implementation).
- Route modules organized by area: `routes::{status, conversation, trust, standing_orders, providers, memory, autonomy, ...}`.

Public API: `serve(state_dir, port)`, `register_routes() -> Router`.

### ghost-cli

Responsibility: `ghost` binary (clap), mirrors `control_plane/cli.py`.

Subcommands: `ask`, `trust {status,runs,trace,replay,eval,eval-cases}`, `standing-orders {list,create,enable,disable,run}`, `console`, `setup`, `doctor`, `capabilities`, `model`, `policy`, `conversation`, `run`, `batch`, `mcp`, ... (full parity list in Phase 3).

Bug #3 lives here: `_run_ask_cli` equivalent MUST `create_run(objective, source="ask-cli")` before executing and `record_step` per execution, and MUST include `trust_run_id` in the JSON output.

### ghost-desktop

Responsibility: the desktop application shell. See Section 3 for the Tauri-vs-Electron decision.

- Bundles the `ghost-console-api` server (sidecar or in-process) + the web UI (existing `static/` assets ported, or rewritten UI).
- Installer targets per Section 7.

---

## 2. Module mapping (Python → Rust)

Surveyed from `~/workspace/ghost-chimera/ghostchimera/`. Every top-level module assigned:

| Python module | Rust crate | Notes |
|---|---|---|
| `config.py` | `ghost-core` | `GhostConfig`, env loading |
| `redaction.py` | `ghost-core` | secret redaction |
| `logging_config.py` | `ghost-core` | `tracing` subscriber setup |
| `sdk.py` | `ghost-core` (+ `ghost-pilot`) | `GhostClient` becomes a Rust API + optional PyO3 bridge (Section 5) |
| `trust_runtime.py` | `ghost-trust` | full port, format-compatible (Section 4) |
| `capability_admission.py` | `ghost-safety` | admission lifecycle |
| `capability_pack.py` | `ghost-safety` | |
| `production_gaps.py` | `ghost-console-api` | operator-facing gap report |
| `superiority.py` | `ghost-console-api` | status payload |
| `stealth/` (loop, events, event_bus, context, experience, prediction, intervention, attention, hosts, approvals, hooks, budgets, governance, eval, cdp, computer, perception, intent, eve_model, nemotron_reasoning, bpo_store, graduation, agent_prompt) | `ghost-stealth` | full port; hosts.py → `HostAdapter` trait impls |
| `chimera_pilot/` (kernel, backends/*, backend_registry, executor, agent_loop, autonomy*, batch_runner, compiler, credential_pool, cron_scheduler, delegate_tool, desktop_*, error_classifier, planner pieces) | `ghost-pilot` | kernel + backends; cron → `tokio-cron-scheduler` |
| `model_layer/` (base_provider, openai_compatible_providers, llm, multi_llm, free_router, cost_monitor, provider_auth, auth_profiles, model_catalog, model_discovery, local_model_inventory, llamacpp_runtime, minimind_*, gemini_provider, claude_agent_provider, codex_cli_provider, opencode_cli_provider, lobster_trap_provider, media_providers) | `ghost-providers` | trait + OpenAI-compat client first (Section 5) |
| `tool_layer/` (file_system, shell, web_search, browser, browser_workspace) | `ghost-tools` | |
| `memory_layer/` + `personalization/` | `ghost-memory` | SQLite via rusqlite |
| `safety_layer/` (approval, audit, gating, lobster_trap, material_policy, policy_enforcement, production, rate_limiter, security_monitor, ssrf) | `ghost-safety` | |
| `control_plane/console.py` + `conversation.py` | `ghost-console-api` | axum; 191 `/api/` paths |
| `control_plane/cli.py` | `ghost-cli` | clap |
| `control_plane/standing_orders.py` | `ghost-console-api` (`standing_orders` module; `ghost-cli` calls it) | single implementation, two surfaces |
| `control_plane/` (setup_wizard, doctor, model_picker, local_model_cli, run_jobs, autonomy pieces, live_presence, local_voice, host_execution, browser_debug, flow_dictation, evolution, latency, parallel_cli, cli_policy, colors, config) | `ghost-cli` / `ghost-console-api` | split by surface: interactive operator tooling → cli; served state → console-api |
| `control_plane/static/` (app.js 6906 lines, index.html, styles.css) | `ghost-desktop` (UI assets) | port or rewrite; dedup fix is load-bearing (Section 6) |
| `agent_core/` | `ghost-pilot` | planner/executor pieces merge into pilot |
| `cognition_layer/` (confidence, hallucination, reasoning, trust, workspace) | `ghost-pilot` | reasoning support |
| `skill_layer/` | `ghost-pilot` | skill registry for the agent loop |
| `mcp/` (client, server, mcp_protocol, normalization) | `ghost-pilot` | `rmcp` crate for MCP client; server via axum or rmcp |
| `connectors/` | `ghost-console-api` | console route adapters |
| `integrations/` (github_*, email, bluesky) | `ghost-console-api` | operator integrations |
| `evals/` + `harness/` | `ghost-pilot` (dev) | eval runner as `cargo xtask`-style tool |
| `observability/` | `ghost-core` | tracing/langsmith exporter |
| `sandbox/` | `ghost-tools` | journey sandbox |
| `saas/` | **dropped** | SaaS/OIDC/RBAC is out of scope for the local-first rebuild [NEEDS INAN if he wants it back] |

---

## 3. IPC / desktop shell architecture

### How the backend talks to the UI

The console backend (`ghost-console-api`, axum) serves HTTP on loopback (`127.0.0.1`, ephemeral or fixed port like today, e.g. `8766`), exactly as the Python console does now. The desktop shell loads the UI from that local server. This preserves:

- The entire existing API surface (191 `/api/` paths) without redesign.
- The ability to run headless (`ghost console --no-open`) for servers.
- A clean process boundary: the UI can be reloaded/updated independently of the backend.

Alternative considered: in-process backend via Tauri commands or Electron `ipcMain` handlers. Rejected for Phase 1 because it would force an API redesign and couples UI/backend release cycles. The loopback-HTTP design is also what makes a future mobile/remote UI possible without backend changes.

Security: bind loopback only, require a per-install console token (already exists in Python: token auth when configured), generate the token on first run and store it in the OS keychain.

### Electron vs Tauri — honest comparison for THIS product

| Dimension | Electron | Tauri |
|---|---|---|
| Binary size | ~180-220 MB (bundles Chromium + Node) | ~8-15 MB (uses OS webview) |
| Memory at idle | ~300-500 MB | ~80-150 MB |
| Distribution | electron-builder: NSIS (Win), dmg (mac), AppImage/deb (Linux); mature auto-update via electron-updater | tauri-bundler: NSIS/WiX (Win), dmg/app (mac), AppImage/deb/rpm (Linux); built-in updater |
| Backend language fit | Backend must be a sidecar binary or Node addon; real IPC is JS↔sidecar over stdio/HTTP anyway | Backend is Rust natively; can link `ghost-console-api` as a library in the same process later |
| Security surface | Full Chromium + Node in the renderer; larger CVE surface | OS webview; smaller surface, but webview version varies by OS |
| Windows WebView2 | N/A (own Chromium) | Requires WebView2 runtime (preinstalled on Win10 1803+/Win11; bootstrapper handles the rest) |
| Developer familiarity | Inan knows Electron-adjacent JS well | Rust-first; Inan is a Rust developer (AXIOM-AETHER) |
| Local-first/privacy story | Fine, but shipping a whole browser for a local dashboard is heavy | Stronger story: tiny binary, OS-native, no bundled browser |
| Build complexity in CI | Mature, well-documented | Good, but Windows code-signing + WebView2 bootstrapper adds steps |

**Recommendation: Tauri.** Reasons specific to this product: (1) the entire backend is being rewritten in Rust — Tauri lets the backend eventually live in-process instead of as a sidecar, which is the natural end state; (2) single small binary matters for a local-first privacy product Inan will distribute himself; (3) Inan's Rust fluency makes Tauri's Rust-side code a strength, not a risk; (4) auto-update, installers, and signing are all solved in the Tauri toolchain.

**However:** Inan explicitly asked for Electron. The loopback-HTTP architecture makes the shell swappable — the backend does not care which shell loads it. So the cost of starting with Electron and moving to Tauri later is low (only the shell + packaging changes).

[NEEDS INAN] Final call: **Tauri (recommended)** vs **Electron (as requested)**. If Electron: backend runs as a sidecar binary spawned by the Electron main process, communicating over loopback HTTP; packaging via electron-builder; auto-update via electron-updater.

---

## 4. Trust Runtime in Rust

### Journal format: keep JSONL hash-chained, field-compatible with Python

**Decision: keep the exact JSONL hash-chained journal format.** Reasons:

1. **Migration.** Inan has existing state in `~/.ghostchimera/trust_runtime/` from the Python version. A format break orphans his journals, approvals, eval baselines, and durable sessions. The Rust `TrustRuntimeStore` must read Python-written journals and vice versa during transition.
2. **Auditability.** JSONL is human-inspectable; the hash chain (`previous_hash`/`record_hash` per record) is the tamper-evidence mechanism. Reimplement the identical chain: `record_hash = sha256(canonical_json(record_without_hashes) + previous_hash)`.
3. **Simplicity.** No schema migration tooling needed.

Field compatibility: Rust serde structs use `#[serde(rename_all)]` only where Python's `to_dict()` keys demand it; add golden-file tests with real Python-generated journal fixtures (checked into `ghost-trust/tests/fixtures/`).

### SQLite vs JSONL

**Decision: JSONL journals (unchanged) + SQLite for the run index.**

- Journals stay JSONL: append-only, crash-safe (fsync per append), hash-chained.
- The `run_index.json` becomes a SQLite database (`trust_index.sqlite`, WAL mode): the Python version rewrites the whole JSON index on every run creation, which is a known durability/perf wart. SQLite gives atomic upserts and concurrent readers.
- Durable sessions (`sessions.json`) move to SQLite as well (same crash-consistency argument).
- Everything else (approvals, eval cases, eval baselines) keeps its current file layout for compatibility, migrated to SQLite only if profiling justifies it.

---

## 5. Provider strategy

### Trait-based system

```rust
// crates/ghost-providers/src/traits.rs
pub trait ModelProvider: Send + Sync {
    fn name(&self) -> &str;
    fn chat(&self, system: &str, user: &str) -> Result<String, ProviderError>;
    fn validate_config(&self) -> Vec<String>;
    fn is_available(&self) -> bool; // credentials present
}
```

Plus `ProviderRegistry::from_env()` that constructs every provider whose credentials are present, and `FreeRouter` for quota-aware routing (with the PR #109 atomic reservation→usage fix ported as a `Mutex`-guarded state transition — in Rust this is naturally enforced by holding the lock across the transition).

### Port order

1. **`OpenAiCompatibleProvider` first** — one `reqwest` client, 24 provider configs (base URL + key env var + default model). This covers OpenAI, Groq, xAI, Mistral, DeepSeek, Together, OpenRouter, Perplexity, Fireworks, Cerebras, AI21, HuggingFace Inference, Nvidia, Nebius, Moonshot, DeepInfra, Qwen, Volcengine, StepFun, GLM, Venice, Vultr, Cloudflare, Gemini-via-OAI-endpoint (the base class plus its 23 subclasses; Ollama, Cohere, LMStudio, and Pollinations subclass BaseProvider directly). This is ~85% of the provider value in one implementation.
2. **Anthropic + Ollama + LMStudio** — native protocols, high user demand (local-first story needs Ollama/LMStudio).
3. **CLI bridges** (`CodexCliProvider`, `OpenCodeCliProvider`, `ClaudeAgentProvider`) — subprocess-based, easy in Rust via `tokio::process`.
4. **Long tail** (Cohere, Pollinations, LlamaCpp HTTP, MiniMind, LobsterTrap, media providers) — port on demand.

### Bridging strategy during transition

The Python providers keep working while the Rust core is built:

- **Phase 1-3:** Rust is standalone; Python remains the daily driver. No bridge.
- **Phase 4 (console cutover):** the Rust console needs providers before the Rust provider set is complete. Bridge: `PythonBridgeProvider` in `ghost-providers` that shells out to `python3 -m ghostchimera providers chat --provider <name>` (a thin new Python CLI subcommand) and parses stdout JSON. This is explicitly a transition shim, deleted in Phase 6. It is marked `#[deprecated]` from birth and never used when a native Rust provider exists.
- **SDK compat:** a PyO3 module (`ghost-py`) exposing `GhostClient` is optional and deferred [NEEDS INAN if any downstream depends on the Python SDK].

---

## 6. Bug-fix port checklist

Each bug: where the fix lives in Rust, and the test that proves it.

### Bug 1 — OpenClawAdapter.install() was a stub
- **Lives in:** `crates/ghost-stealth/src/hosts/openclaw.rs`, `impl HostAdapter for OpenClawAdapter::install()`.
- **Fix:** return a real `InstallPayload { host: "openclaw", installed: true, context_engine_snippet: { lifecycle_hooks: { ingest/assemble/after-turn → "ghost-hook openclaw-*" } }, capabilities: {...} }`. The `HostAdapter` trait has no default `install()` — every adapter must implement it (compile-time enforcement; the stub is unrepresentable).
- **Test:** `hosts::tests::openclaw_install_returns_lifecycle_hooks` — asserts `installed == true`, all three hooks present, commands start with `ghost-hook`.

### Bug 2 — Console chat rendered duplicate Ghost replies
- **Lives in:** `crates/ghost-console-api/src/conversation.rs` (controller) + desktop UI render logic.
- **Fix:** the reply endpoint returns `{ reply, transcript }`; the UI renders the reply box ONLY when `reply != last transcript turn`. Port the exact dedup condition from PR #117's `app.js` fix. Additionally, the API stops sending `last_reply` redundantly when it equals the final turn (belt and suspenders).
- **Test:** `conversation::tests::no_duplicate_reply_when_reply_equals_final_turn` (unit) + a UI-level test asserting the reply box shows standby text in that case.

### Bug 3 — `ghost ask` must journal every run
- **Lives in:** `crates/ghost-cli/src/cmd/ask.rs`.
- **Fix:** `run_ask()` calls `trust.create_run(objective, source="ask-cli")` BEFORE kernel execution, `record_step` per `PilotExecution` (ok/error with backend_id + truncated output), and injects `trust_run_id` into the JSON output. On `PermissionError`-equivalent, records a `run_failed` step. This is a code-review checklist item: any new CLI entrypoint that executes objectives must journal (enforced by a `#[must_use]`-style wrapper `JournaledRun` — you cannot get executions without a run id).
- **Test:** `cmd::ask::tests::ask_journals_trust_run` — runs `ask` against a temp state dir, asserts the journal file exists with `run_created` + `task_executed` steps and the output contains `trust_run_id`.

### Bug 4 — Standing Orders CLI parity
- **Lives in:** `crates/ghost-console-api/src/standing_orders.rs` (single `StandingOrderStore` implementation) + `crates/ghost-cli/src/cmd/standing_orders.rs` (thin CLI calling the same store).
- **Fix:** one store, two surfaces. CLI supports `list/create/enable/disable/run`; create validates title+objective (error otherwise); orders are disabled-by-default.
- **Test:** `standing_orders::tests::cli_lifecycle` — create→list→enable→run→disable round-trip through the CLI handler functions.

### Bug 5 — Console chat deterministic fallback with honesty
- **Lives in:** `crates/ghost-console-api/src/conversation.rs`, in the objective-execution path.
- **Fix:** when the model-provider backend is unavailable (`BackendUnavailable`), fall back to `DeterministicBackend { fulfill: true }`. If it fulfills, return the result with `note: "Ran on the offline deterministic backend (no model provider configured)."`. If it cannot fulfill, return its honest `deterministic_unfulfilled` error — never a generic failure, never false success.
- **Test:** `conversation::tests::chat_falls_back_to_deterministic_backend` (arithmetic objective, no providers configured → ok + note) and `..._honest_failure_for_unfulfillable` (creative objective → ok=false, error starts with `deterministic_unfulfilled`).

### Bug 6 — Real replay/re-execution
- **Lives in:** `crates/ghost-trust/src/replay.rs`, `TrustRuntimeStore::replay_run(run_id, executor)`.
- **Fix:** loads the journaled objective, creates a linked `source="replay"` run, re-executes (default: deterministic kernel; injectable executor for tests), records `replay_execution` + `replay_comparison` steps, returns `execution_performed: true` + outcome comparison. `simulate_replay` (policy preview) is kept but renamed in docs as preview-only; the README claim "replayable" refers to `replay_run`.
- **Test:** `replay::tests::{replays_real_execution, honest_failure_for_unfulfillable, executor_error_records_replay_failed, outcome_match_logic}`.

### Bug 7 — Deterministic honesty (PR #116)
- **Lives in:** `crates/ghost-pilot/src/backends/deterministic.rs`.
- **Fix:** `fulfill=true` → `_try_fulfill`: file listing via `std::fs::read_dir` (sandboxed: reject paths escaping the workspace root), arithmetic via a safe expression evaluator (hand-written recursive descent or the `meval`-style crate — NOT `eval`). Anything else → `Err(Unfulfilled("deterministic_unfulfilled: ..."))`. `PilotExecution::ok()` is false when `verification_error.is_some()`. The CLI and console both surface `ok=false, task_completed=false`.
- **Test:** `backends::deterministic::tests::{fulfills_file_listing, fulfills_arithmetic, honest_failure_for_poem, rejects_path_traversal}`.

---

## 7. Production-readiness checklist

### Installer targets
- **Windows (primary — Inan's machine):** NSIS installer (Tauri default) or WiX MSI. Per-user install, no admin required. Start Menu shortcut, `ghost` CLI on PATH (optional checkbox).
- **macOS:** `.dmg` (Tauri bundler). Apple Silicon + Intel universal binary.
- **Linux:** AppImage (primary) + `.deb` (secondary).
- [NEEDS INAN] Confirm: Windows-first release, macOS/Linux follow? Or all three day one?

### Code signing
- Windows: EV or standard code-signing cert. Without signing, SmartScreen warns. [NEEDS INAN] Purchase decision (~$200-500/yr standard, ~$400-700/yr EV). Self-signed is free but shows warnings — acceptable for alpha, not for "production-ready".
- macOS: Apple Developer Program ($99/yr) + notarization. Required for non-warning installs.
- Linux: GPG-sign the AppImage/deb.

### Auto-update
- Tauri: built-in updater (signed update bundles from GitHub Releases).
- Electron: electron-updater against GitHub Releases.
- Update channel: `stable` only for v1.0. No beta channel until asked.

### Crash reporting
- Rust: `human-panic` for the CLI (writes a crash report file to the state dir, shows a friendly message). Desktop: unhandled-error dialog with "report saved to <state_dir>/crashes/".
- No third-party telemetry (Sentry etc.) — local-first product; crash reports stay on disk. [NEEDS INAN if he wants opt-in telemetry later.]

### Logging
- `tracing` + `tracing-subscriber`: JSON logs to `<state_dir>/logs/ghost-%Y-%m-%d.log`, rotation by day, keep 14 days. `RUST_LOG` / `GHOST_LOG` env override. Console token and secrets never logged (redaction in `ghost-core`).

### Config/state directories per OS (via `directories` crate)
- Windows: `%APPDATA%/ghost-chimera/` (config), `%LOCALAPPDATA%/ghost-chimera/` (state, logs).
- macOS: `~/Library/Application Support/ghost-chimera/`.
- Linux: `~/.config/ghost-chimera/` + `~/.local/share/ghost-chimera/`.
- Migration: on first run, if `~/.ghostchimera/` (Python layout) exists and the new location is empty, import it (copy, never move; journals are read-compatible per Section 4).
- `GHOSTCHIMERA_STATE_DIR` env var still overrides everything (operator/CI escape hatch).

### Versioning
- SemVer, starting at `0.5.0` (Python is `0.4.0-beta`; the Rust rebuild is a minor bump, not 1.0 — 1.0 waits for the production checklist to be fully green).
- Version in one place: workspace `Cargo.toml` (`workspace.package.version`), embedded via `env!("CARGO_PKG_VERSION")`.

### Release pipeline (GitHub Actions)
- `.github/workflows/rust-ci.yml`: `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test --workspace`, `cargo mutants` (nightly or weekly schedule — full mutation on every PR is too slow; gate PRs on changed-crate mutation, run full weekly).
- `.github/workflows/release.yml`: on tag `v*`: build matrix (windows-latest, macos-latest, ubuntu-latest), sign (Windows/macOS), build installers, create GitHub Release with artifacts, publish update manifest (`latest.json` for the updater).
- `cargo deny` for supply-chain (licenses + advisories) in CI.
- `cargo audit` in CI.

---

## 8. Test strategy

### What gets ported (priority order)

Critical path first — these suites encode the product's correctness contract:

1. **Trust runtime** (`test_trust_runtime*.py`, 3 files) → `ghost-trust/tests/`. Journal integrity, approvals, eval baselines, replay. Golden-file fixtures from Python journals.
2. **Deterministic honesty** (`test_ask_cli.py`, `test_readme_hardening.py`, honesty tests from #116) → `ghost-pilot/tests/` + `ghost-cli/tests/`.
3. **Stealth loop + hosts** (`test_stealth_*.py`, 8 files) → `ghost-stealth/tests/`. The EVENT→…→LEARN wiring test is the highest-value single test.
4. **Conversation/dedup** (`test_conversation*.py`, 8 files) → `ghost-console-api/tests/`.
5. **Standing orders** (`test_standing_orders*.py`, 2 files) → `ghost-console-api/tests/`.
6. **Providers** (`test_provider*.py`, `test_model*.py`) → `ghost-providers/tests/` (mock HTTP via `wiremock`).
7. **Safety/policy** → `ghost-safety/tests/`.
8. Long tail (github integrations, evals, saas — saas dropped) ported opportunistically or not at all; behavior covered by API-level tests instead.

### Rust test layout

- Unit tests: `#[cfg(test)] mod tests` inside each module (fast, white-box).
- Integration tests: `crates/<crate>/tests/*.rs` (black-box, one file per ported Python suite, named `py_<suite>.rs` with a header comment citing the source file).
- Fixtures: `crates/<crate>/tests/fixtures/` (Python-generated journals, provider HTTP mocks).

### Mutation testing in CI

- `cargo-mutants` (Inan's standing rule: Rust gate runs in CI, never claimed green from sandbox).
- PR gate: `cargo mutants --changed` (only mutants in changed code), fail under 80% killed — mirrors the Python `testteeth --fail-under 80` policy.
- Weekly full-workspace mutation run; results tracked, not gated (too slow to gate).

---

## 9. Phased build plan

### Phase 1 — Foundation (crates: ghost-core, ghost-trust, ghost-pilot skeleton)
- Workspace + `ghost-core` (config, errors, redaction, ids).
- `ghost-trust`: `TrustRuntimeStore` with create_run/record_step/get_run/list_runs + JSONL hash chain, golden-file compatible with Python journals.
- `ghost-pilot`: `TaskSpec`, `TaskKind`, `Backend` trait, `BackendRegistry`, `DeterministicBackend` (fulfill=true/false), `ChimeraPilotKernel::run`.
- **Done when:** `cargo test` green; golden-file test reads a Python-written journal; deterministic backend passes the bug-7 test set; `clippy` + `fmt` clean.

### Phase 2 — Providers + tools + safety
- `ghost-providers`: `ModelProvider` trait, `OpenAiCompatibleProvider` (24 configs), Anthropic, Ollama, LMStudio, `FreeRouter` (atomic quota fix), `CostMonitor`.
- `ghost-tools`: fs/shell/web tools with trust envelopes.
- `ghost-safety`: policy engine, approval gate, SSRF guard, rate limiter, HMAC audit log.
- **Done when:** provider tests pass against `wiremock`; a `chat()` call round-trips through a mock OpenAI-compatible server; safety tests prove SSRF-blocked egress and approval-required mutating tools.

### Phase 3 — CLI parity
- `ghost-cli`: `ask` (with trust journaling — bug 3), `trust` (status/runs/trace/replay/eval), `standing-orders` (bug 4), `setup`, `doctor`, `model`, `policy`, `console`, `run`, `batch`.
- `replay_run` in `ghost-trust` (bug 6).
- **Done when:** every bug-3/4/6/7 test green; `ghost ask "what is 2+2" --json` journals and prints `trust_run_id`; `ghost trust replay <id>` re-executes.

### Phase 4 — Console API
- `ghost-console-api`: axum server, route groups ported from `console.py` (status, conversation, trust, standing_orders, providers, memory, autonomy…), `ConversationalLoopController` with dedup (bug 2) + deterministic fallback (bug 5), `StandingOrderStore`.
- `ghost-memory`: SQLite memory store + ingestion.
- `ghost-stealth`: StealthLoop + all six host adapters with real `install()` (bug 1).
- **Done when:** all bug-1/2/5 tests green; console serves the UI bundle; the `/api/` paths respond (contract tests assert path presence + shape).

### Phase 5 — Desktop shell + installers
- `ghost-desktop`: Tauri (or Electron — [NEEDS INAN]) shell loading the loopback console; sidecar or in-process backend.
- Installers: Windows NSIS, macOS dmg, Linux AppImage (+deb).
- Auto-update wiring, crash reporting, per-OS state dirs, first-run migration from `~/.ghostchimera`.
- **Done when:** installer builds in CI for all three OSes; fresh install → launch → console opens → `ask` works; auto-update check hits the release manifest.

### Phase 6 — Hardening + cutover
- Full test-suite port completion, weekly mutation baseline, `cargo deny`/`audit` clean.
- Python bridge provider removed (if it was ever added).
- Docs: user install guide, operator runbook, migration notes.
- **Done when:** production-readiness checklist (Section 7) fully green; Inan declares the cutover. Version `0.5.0` tagged.

Each phase is independently verifiable via its "Done when" criteria. No phase starts until the previous phase's criteria are met.

---

## 10. Risks

### R1. Provider ecosystem rewrite cost
28 provider classes is the single largest port surface. **Mitigation:** the OpenAI-compatible client collapses 24 providers into one implementation + config table (verified: the base class plus 23 subclasses in `openai_compatible_providers.py`). The `PythonBridgeProvider` shim covers the long tail during transition and is deleted in Phase 6.

### R2. Behavioral drift from Python
78k lines of subtle behavior (redaction rules, journal shapes, dedup conditions) will drift in a rewrite. **Mitigation:** golden-file tests generated from the Python implementation (journals, API payloads, CLI outputs) checked into the repo; the bug-fix checklist (Section 6) is a hard gate per phase; `simulate_replay` semantics documented as preview-only so nobody reintroduces the "replayable" overstatement.

### R3. SQLite migration of the run index
Moving `run_index.json` → SQLite changes the durability story mid-flight. **Mitigation:** journals stay JSONL (the source of truth); the SQLite index is rebuildable from journals (`trust rebuild-index` CLI command). Corruption of the index is recoverable by design.

### R4. Desktop packaging on Windows (Inan's machine)
Code signing cost, WebView2 bootstrapper (Tauri) or 200MB installer (Electron), SmartScreen reputation for a new publisher. **Mitigation:** ship unsigned alpha first for Inan's own testing; signing cert decision is [NEEDS INAN]; Tauri keeps the download small which matters for SmartScreen reputation building.

### R5. The rewrite stalls the Python line
A full rebuild takes months; the Python version still needs maintenance and Inan's other work (job hunt) competes for time. **Mitigation:** phased plan with independently shippable milestones; Python `main` stays the supported line until Phase 6 cutover; each Rust phase is time-boxed and its "Done when" criteria are the stop/go gate. If a phase slips twice, descope to the critical-path test list (Section 8) rather than extending.

---

## Appendix A. Decisions for Inan

- [NEEDS INAN] **Desktop shell: Tauri (recommended) vs Electron (requested).** Architecture is shell-agnostic (loopback HTTP), so this can be revisited.
- [NEEDS INAN] **Installer day-one targets:** Windows-first, or all three OSes?
- [NEEDS INAN] **Code signing:** purchase Windows cert ($200-700/yr) + Apple Developer ($99/yr), or ship unsigned alpha first?
- [NEEDS INAN] **SaaS crate:** dropped from the rebuild (local-first). Confirm.
- [NEEDS INAN] **PyO3 SDK bridge:** does anything downstream depend on `ghostchimera.sdk` from Python?
- [NEEDS INAN] **Opt-in telemetry:** default is no telemetry, crash reports stay on disk. Confirm.

## Appendix B. Key file references (Python source)

- Trust runtime: `ghostchimera/trust_runtime.py` (1193+ lines; `TrustRuntimeStore`, hash chain, `replay_run` at :791, `simulate_replay` at :712)
- Pilot kernel: `ghostchimera/chimera_pilot/kernel.py` (`ChimeraPilotKernel::default` at :76, `deterministic_fulfill` at :80)
- Deterministic backend: `ghostchimera/chimera_pilot/backends/deterministic.py` (`_try_fulfill`, `UNFULFILLED_ERROR`)
- Providers: `ghostchimera/model_layer/openai_compatible_providers.py` (28 classes: base + 23 OpenAI-compatible subclasses + 4 direct BaseProvider subclasses), `base_provider.py` (minimal ABC)
- Stealth loop: `ghostchimera/stealth/loop.py` (`StealthLoop`), `hosts.py` (6 adapters; `OpenClawAdapter` at :242)
- Console: `ghostchimera/control_plane/console.py` (191 `/api/` paths, `register_console_routes` at :905, `_default_run_objective` at :641), `conversation.py` (`ConversationalLoopController`), `standing_orders.py`, `static/app.js` (6906 lines; dedup fix)
- CLI: `ghostchimera/control_plane/cli.py` (`_run_ask_cli` at :960, `_TOP_LEVEL_COMMANDS`)
- Config: `ghostchimera/config.py` (`GHOSTCHIMERA_STATE_DIR`, default `~/.ghostchimera`)
- Safety: `ghostchimera/safety_layer/` (audit HMAC-SHA256 verified)
- Free router quota fix (PR #109): `ghostchimera/model_layer/free_router.py`
