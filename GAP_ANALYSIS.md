# GHOST-Chimera Gap Analysis

**Date:** 2026-10-11
**Scope:** Python codebase at `~/workspace/ghost-chimera` @ `68b70a8` (~78k lines, 275 files)
**Method:** Five parallel code-reading hunters (README truth, honesty, durability, security, drift). Every finding was reproduced or code-confirmed before reporting; automated-overlap findings were discarded.
**Feeds:** Rust rebuild Phase 5 (bug-fix verification). Each finding carries a Rust crate assignment per `SPEC.md`.

**Already tracked, not re-reported** (the 7 known bugs, fixed in Python PRs #116/#117):
1. `OpenClawAdapter.install()` stub — 2. console chat duplicate Ghost replies — 3. `ghost ask` not journaling — 4. Standing Orders had no CLI — 5. console chat had no deterministic fallback — 6. preview-only replay — 7. false-success reporting in `ask` (PR #116 honesty).

## Counts

| Severity | Count |
|---|---|
| Critical | 6 |
| Major | 19 |
| Minor | 27 |
| **Total** | **52** |

Severity was normalized across hunters (durability HIGH→critical, MEDIUM→major, LOW→minor).

---

## Critical (6)

### [security/C1] Console binds 0.0.0.0 with zero auth and no fail-closed guard
- **Evidence:** `ghostchimera/control_plane/console.py:966` (`_api_auth = "token" if console_token else "open"`); `cli.py:168,176` accepts `--host 0.0.0.0` with no token requirement; `docker-compose.saas.yml:27` ships `ghostchimera console --host 0.0.0.0 --http-port 8766` published on `8766:8766` with no auth token.
- **Impact:** ~240 API routes (run objectives, queue autonomy jobs, browser fetch, approval resolution) exposed to the network unauthenticated.
- **Rust crate:** `ghost-console-api` — fail-closed: refuse non-loopback bind without a token.

### [honesty/F1] Stealth loop records SUCCESSFUL execution when executor is None
- **Evidence:** `ghostchimera/stealth/loop.py:797-816` (`handle_agent_output`). With an L3 ACT decision and `executor=None`, each action is appended `{"ok": True, "result": None}`, intervention transitions EXECUTED→CONSUMED, outcome recorded SUCCESSFUL, and the workflow graph + learner log it as useful. Nothing executed. Reproduced live.
- **Impact:** Fabricated success poisons the learning loop (workflows marked useful that never ran).
- **Rust crate:** `ghost-stealth` — executor mandatory for ACT decisions; never record SUCCESSFUL on no-op.

### [honesty/F2] `run_order` reports ok:true and bumps run_count on a no-op preview
- **Evidence:** `ghostchimera/control_plane/standing_orders.py:160-187`. With `objective_runner=None`, returns `{"ok": True, "preview": True}`, increments `run_count`, stamps `last_run_at`, fires `standing_order_run`. Reproduced. The CLI `standing-orders run` (`cli.py:1547`) calls it with no runner, so it always takes this path.
- **Rust crate:** `ghost-pilot` — preview must return `executed: False`, never bump counters.

### [honesty/F3] Remote approval marked `executed` with no execution
- **Evidence:** `ghostchimera/integrations/remote_control.py:1366-1379` (`resolve_approval`). With `objective_runner=None`, approval status is written `"executed"`, a `remote_approval_executed` event fires, `ok:true` returned. Only the nested result hints `"skipped": True`. Reachable via `/approve` inbound commands (`remote_control.py:1076,1203`). Reproduced.
- **Rust crate:** `ghost-console-api` — `"approved"` (decision) vs `"executed"` (post-run) must be distinct states.

### [durability/H1] CostLedger never persisted in production
- **Evidence:** `ghostchimera/model_layer/cost_monitor.py:342` (module-level `_ledger` singleton). `save()`/`load()` exist and are tested but have **zero production callers**. All spend, call counts, daily rollups, latency samples, and `set_budget()` caps are lost on restart.
- **Impact:** Budget enforcement silently resets to uncapped after restart.
- **Rust crate:** `ghost-providers` — durable ledger + flush policy (product decision: periodic vs on-record vs shutdown hook).

### [readme/C1] One-line installers fail on their own default path
- **Evidence:** `scripts/install.sh:10-13`, `scripts/install.ps1:8` default to lean extras `gateway,dev`, but the verification step requires `pyautogui` (`install.sh` tail; `install.ps1:65-76`), which only ships with `desktop`/`all` extras (`pyproject.toml:42,66`). Fresh default install raises `SystemExit("Missing installed runtime dependencies: pyautogui")` and aborts.
- **Impact:** The README's advertised entry path is broken out of the box.
- **Rust crate:** `ghost-cli` — installer/extras coherence (fix in Python too, or ensure the Rust installer doesn't repeat it).

---

## Major (19)

### [security/M1] Provider API keys stored PLAINTEXT in config.json
- **Evidence:** `ghostchimera/control_plane/setup_wizard.py:287,302` writes `config["model"]["api_key"]` verbatim; `config.py:43-62` protects with POSIX 0600 only. README:165 claims "secrets encrypted".
- **Rust crate:** `ghost-providers` (encrypted credential store) / `ghost-core`.

### [security/M2] Fernet vault key falls back to a hardcoded constant
- **Evidence:** `ghostchimera/connectors/auth_engine.py:166-170`: `sha256(b"ghost-chimera-local-dev-vault")` unless `GHOSTCHIMERA_VAULT_SECRET` is set (undocumented in wizard). Every "encrypted" OAuth token is decryptable from public source.
- **Rust crate:** `ghost-core` / `ghost-safety`.

### [security/M3] Audit log writes unredacted task payloads to disk
- **Evidence:** `ghostchimera/safety_layer/audit.py:125-134` appends `{"task": task, "result": result}` with no redaction; callers in `agent_core/executor.py:46-62` include shell commands and file writes that may embed secrets.
- **Rust crate:** `ghost-safety` — `AuditLog::record` must redact first.

### [security/M4] SSRF: no private-IP/loopback block on the actual fetch paths
- **Evidence:** `tool_layer/browser.py:24-56` (`http_get`) enforces HTTPS only; `SSRFPolicy` (`chimera_pilot/policy.py:96-107`) only runs when `allowed_hosts` is non-empty (default empty). Reachable via open `POST /api/console/browser/fetch`.
- **Rust crate:** `ghost-safety` — wire `SsrfGuard` into the default path.

### [security/M5] Redaction markers/patterns miss common secret shapes
- **Evidence:** `ghostchimera/redaction.py:17-24`: no bare `"key"` marker; patterns miss `AIza…`, `xai-…`, `gsk_…` in free text under neutral keys.
- **Rust crate:** `ghost-core` — widen `redact_value`.

### [honesty/F4] `resume_run` marks run "running" but nothing resumes it
- **Evidence:** `ghostchimera/trust_runtime.py:525-562`: flips run index to `"running"`, mints a `RunResumeToken`, returns ok:true. **No code path consumes the token.** The run sits "running" forever.
- **Rust crate:** `ghost-trust` — wire a real resume executor or refuse explicitly.

### [durability/M1] ConversationStore: non-atomic write + silent total wipe on corrupt file
- **Evidence:** `control_plane/conversation.py:237` (`_save` is direct `write_text`, no tmp+replace); `_load` catches `JSONDecodeError` and returns default state — silently discarding ALL sessions, settings, and pending approvals after a torn write.
- **Rust crate:** `ghost-console-api` — atomic write + corrupt-file quarantine.

### [durability/M2] AutonomyJobQueue: "running" jobs orphaned forever after restart
- **Evidence:** `chimera_pilot/autonomy_queue.py:180`. Job records durable in `autonomy/jobs.json`, but no recovery sweep: `"running"` at shutdown stays `"running"` forever; `run_next()` only picks `"queued"`.
- **Rust crate:** `ghost-pilot` — startup recovery pass (requeue or mark-interrupted).

### [durability/M3] Stealth BackgroundRuntime built with no queue_file
- **Evidence:** `stealth/loop.py:104` constructs `BackgroundRuntime()` with no args; `stealth/runtime.py:67` defaults `queue_file=None`, disabling its JSONL persistence entirely. Stealth background jobs purely in-memory.
- **Rust crate:** `ghost-stealth` — durable job journal wired by default.

### [durability/M4] ConsoleRunManager: background console runs have no persistence
- **Evidence:** `control_plane/run_jobs.py:55`; constructed at `console.py:937` with no state_dir. In-flight runs die on restart (daemon threads), no record left.
- **Rust crate:** `ghost-console-api`.

### [durability/M5] FreeRouter._save silently swallows write errors
- **Evidence:** `model_layer/free_router.py:267` (`except OSError: pass`). Quota usage IS durable on the happy path (verified — restart does not reset quotas), but a failed write is invisible and quotas reset next restart.
- **Rust crate:** `ghost-providers` — loud failure or retry on usage-write errors.

### [durability/M6] CalibrationStore: backend health history explicitly in-memory
- **Evidence:** `chimera_pilot/calibration.py:18` ("Bounded in-memory calibration history"). After restart, `reliability()` falls back to 0.5 neutral — scheduler picks backends blind until re-probed; learned flakiness forgotten.
- **Rust crate:** `ghost-pilot`.

### [durability/M7] AIAgent SessionState.save()/load() exist but are never called
- **Evidence:** `chimera_pilot/agent_loop.py:312` (`self._sessions` dict); `save`/`load` at :165/:170 delegate to TrustRuntimeStore but have zero callers. (Gateway `GatewaySession`s ARE durable — asymmetry is easy to miss.)
- **Rust crate:** `ghost-pilot` — wire to trust journal or drop the dead API.

### [readme/M1] README misstates the installer default extras
- **Evidence:** README claims `GHOSTCHIMERA_EXTRAS` "defaults to the full Ghost Chimera runtime"; `scripts/install.sh:7-13` and `install.ps1:5-8` say "Lean default: gateway + dev tooling".
- **Rust crate:** `ghost-cli`.

### [readme/M2] "Morning briefings, meeting prep" do not exist
- **Evidence:** Zero hits for "briefing"/"meeting prep" in `ghostchimera/`. Only a daypart classifier in `stealth/temporal.py:41-45`.
- **Rust crate:** `ghost-core` (build it or drop the claim).

### [readme/M3] "Dry-runnable before anything touches the world" is desktop-only
- **Evidence:** Only `DesktopRuntimeBackend` has dry-run (`chimera_pilot/backends/desktop_runtime.py:37`; `kernel.py:153`). No `--dry-run` on pilot CLI, none in `executor.py`, none for browser/computer-use.
- **Rust crate:** `ghost-pilot` — dry-run must be a kernel-level capability.

### [drift/Major-1] `ghostchimera worker start` never starts a worker
- **Evidence:** `saas/cli.py:78-99` prints `"mode": "dry-run"` / "Production workers require SaaS Postgres configuration", exits 0. `WorkerQueue` has no run loop. `docker-compose.saas.yml`'s worker service runs this command — the container exits immediately. Docs list it as a launch procedure (`docs/PUBLIC_LAUNCH_SAAS.md:49`).
- **Rust crate:** none — SPEC already drops `saas/`; do not port.

### [drift/Major-2] `ghostchimera saas create-admin` creates nothing durable
- **Evidence:** `saas/cli.py:33-52` writes to `InMemorySaasStore` (docstring: "used by tests, local demos, and CLI smoke checks", `saas/store.py:134-136`), prints `"ok": True`. Admin evaporates on exit.
- **Rust crate:** none — dropped with saas; do not port.

### [drift/Major-3] `ghost-hook` binary does not exist; all six host adapters reference it
- **Evidence:** Every adapter's `install()` emits configs pointing at `ghost-hook` (e.g. `"command": "ghost-hook prompt"`, `hosts.py:172-186`; `ghost-hook openclaw-*`, `:253-275`). No `[project.scripts]` entry, no CLI subcommand, no script file. Installed adapters point at a non-existent executable. `HostAdapter.subscribe()` is a no-op in base, overridden by none.
- **Rust crate:** `ghost-stealth` — FIX: implement the hook binary or point snippets at something real. SPEC asserts hooks start with `ghost-hook` but never plans the binary; the port would replicate the dangling reference.

---

## Minor (27)

### Honesty (3)
- **[honesty/F5]** `run_presence_eval_suite` hardcodes `latency_budget` as passed (`control_plane/live_presence.py:793`): `"passed": True`, `budget_ms: 250`, zero measurement. → `ghost-console-api`.
- **[honesty/F6]** `configure_meeting_bridge` claims `"ready"` + 4 capabilities unverified (`live_presence.py:304-343`): browser_session is an unchecked string. → `ghost-console-api`.
- **[honesty/F7]** `send_communication` marks draft `"sent"` but only journals locally (`live_presence.py:514-529`); payload honestly says `delivery_status: "recorded"`, but the action is called `send`. → `ghost-console-api`.

### Durability (8)
- **[durability/L1]** Token-bucket rate limiters refill on restart (`model_layer/rate_limit.py:125`, `safety_layer/rate_limiter.py:44`); restart loop bypasses pacing. → `ghost-providers` / `ghost-safety`.
- **[durability/L2]** `ChimeraScheduler` weight adaptation never persisted (`chimera_pilot/scheduler.py:98`; `save_weights`/`load_weights` zero callers; drift ±0.01). → `ghost-pilot`.
- **[durability/L3]** `InMemoryTelemetryStore` events + replay bundles lost (`chimera_pilot/telemetry.py:65`). → `ghost-pilot`.
- **[durability/L4]** `CredentialPool` refreshed OAuth tokens memory-only (`chimera_pilot/credential_pool.py:115`); restart forces re-auth. → `ghost-pilot`.
- **[durability/L5]** `ExperienceStream` appended but never persisted (`stealth/eve_model.py:249`; `to_dict` has no writer). → `ghost-stealth`.
- **[durability/L6]** `automations._save` suppresses OSError silently (`connectors/automations.py:86`). → `ghost-console-api`.
- **[durability/L7]** `CronScheduler`: downtime runs silently skipped, no catch-up; result history not kept (`chimera_pilot/cron_scheduler.py`). → `ghost-pilot`.
- **[durability/L8]** `GitHubEventConnector` dedup state in-memory (`connectors/github_events.py:36`); latent (no production wiring today). → `ghost-console-api`.

### README (3)
- **[readme/m1]** "Inbox triage drafts (never auto-sent)" overstates: `triage.py` is pure scoring, produces no drafts; `mail_basic.py` has no send. "Never auto-sent" vacuously true. → `ghost-core`.
- **[readme/m2]** Approvals journaled only on the service path: `ApprovalQueue` in-memory by default (`stealth/approvals.py:126-141`); durability only via attached authority (`connectors/stealth_service.py:116`). Bare loop loses approval history. → `ghost-trust`.
- **[readme/m3]** In-repo Homebrew formula out of sync: placeholder `sha256 "REPLACE_WITH_RELEASE_TARBALL_SHA256"`, no `head` stanza; live tap has it. → `ghost-cli`.

### Drift (4)
- **[drift/Minor-1]** 12 console API routes with no frontend caller (real implementations, just unlinked): `context/compress`, `conversation/local-voice/transcribe`, `host-execution/run`, `host-execution/self-edit`, `production/gaps`, `provider-auth/oauth/poll`, `remote/health`, `runs` (list), `runtime/dependencies`, `voice/flow/history`, plus 2 legit OAuth callbacks. → port, wire, or drop deliberately (mostly `ghost-console-api`).
- **[drift/Minor-2]** `ghostchimera/sdk.py` (368 lines) has zero production importers. → keep deferred per SPEC.
- **[drift/Minor-3]** `saas init-db` never touches a DB; prints empty sql + `"ok": True` without `--print-sql`. → dropped with saas.
- **[drift/Minor-4]** SPEC Bug 1 premise is stale: Python `OpenClawAdapter.install()` already returns `installed: True` with hooks + capabilities (`hosts.py:253-275`). Update SPEC before porting; the real gap is [drift/Major-3].

### Security (9)
- **[security/m1]** Custom MCP HTTP server (127.0.0.1:3100): no auth, no CORS, `/call` executes tools (`mcp/server.py:88-118`); latent (unused in-repo). → `ghost-console-api` / `ghost-tools`.
- **[security/m2]** No CSRF/CORS on the gateway (`chimera_pilot/gateway_server.py:825+`); drive-by simple POSTs execute on open loopback routes. → `ghost-console-api`.
- **[security/m3]** Approval opt-in per tool; `read_file` lacks `requires_approval` (`chimera_pilot/toolsets.py:131-139`); `GHOSTCHIMERA_AUTO_APPROVE=1` silently approves all (`safety_layer/approval.py:346`). → `ghost-pilot` / `ghost-safety`.
- **[security/m4]** `bpo_store.record_audit` trusts callers to redact; only truncation enforced (`stealth/bpo_store.py:208-220`). → `ghost-stealth`.
- **[security/m5]** Agent sessions stored AND served unredacted by design (`trust_runtime.py:593-647`, `redact=False`); pasted API keys persist verbatim in `sessions.json`. → `ghost-trust` (decide explicitly).
- **[security/m6]** Key previews leak 2–4 chars of real keys in status responses (`console.py:290-295`, `credential_pool.py:430-433`). → `ghost-console-api` / `ghost-providers`.
- **[security/m7]** LangSmith tracing POSTs unredacted prompts to a third party (`observability/langsmith_tracing.py:109-131`). → `ghost-core`.
- **[security/m8]** MCP stdio servers inherit full `os.environ` (`mcp/mcp_protocol.py:189-199`), inconsistent with scrubbed env elsewhere. → `ghost-tools`.
- **[security/m9]** `allow_network_commands=True` default inconsistent (`control_plane/host_execution.py:31`); destructive-desktop confirmation phrase hardcoded (`chimera_pilot/desktop_policy.py:24`). → `ghost-safety` / `ghost-pilot`.

---

## Cross-cutting patterns

1. **Persistence built but never wired** (durability): `CostLedger.save/load`, `ChimeraScheduler.save_weights/load_weights`, `SessionState.save/load` — full APIs, zero production callers. Plus three silent-failure writes (`FreeRouter._save`, `ConversationStore._save`, `automations._save`). The Rust port must wire persistence into the runtime lifecycle, not just provide the API.
2. **Success without execution** (honesty): F1, F2, F3, F4 share one shape — a `None`/missing executor or worker is papered over with `ok:true`. The Rust port should make the executor/worker a required parameter (type-level) wherever a success claim is recorded.
3. **Encryption theater** (security): M1 (plaintext keys vs "secrets encrypted" claim) + M2 (hardcoded Fernet fallback). The Rust `ghost-core` secret store must derive keys per-install and actually encrypt the key classes the README names.
4. **Dangling references** (drift): `ghost-hook` (Major-3), `RunResumeToken` with no consumer (F4), `sdk.py` with no importers. The port should delete or implement — never carry forward.
5. **README as bug source** (readme): C1, M1, M2, M3, m1 are all cases where docs promise what code doesn't do. Phase 5 should include a doc-to-code re-verification pass.

## Verified clean (spot-checked, no finding)

Trust journals redacted on write AND read; no `shell=True` in production paths; Python backend runs with minimal env; host execution requires arming + confirmation + root pinning; capability defaults all False; non-interactive approval auto-denies; `SSRFPolicy` itself correct (just off the default path); all 34 CLI subcommands dispatch to real implementations; every one of 180 frontend fetch paths resolves to a registered route; all six adapters have real `detect()`/`install()`; `TRUST_RUNTIME.md`, `STANDING_ORDERS.md`, `REMOTE_CONTROL.md`, `PUBLIC_LAUNCH_SAAS.md` state boundaries honestly; MemoryStore/TrustRuntimeStore/ActionApprovalStore/LivePresenceStore/StandingOrderStore/SecurityMonitor/cron defs/FreeRouter usage are durable.

## Crate assignment summary

| Crate | Findings |
|---|---|
| ghost-console-api | SEC-C1, SEC-m1, SEC-m2, SEC-m6, HON-F3, HON-F5, HON-F6, HON-F7, DUR-M1, DUR-M4, DUR-L6, DUR-L8, DRF-Minor-1 |
| ghost-pilot | HON-F2, SEC-m3, SEC-m9, DUR-M2, DUR-M6, DUR-M7, DUR-L2, DUR-L3, DUR-L4, DUR-L7, RD-M3 |
| ghost-stealth | HON-F1, SEC-m4, DUR-M3, DUR-L5, DRF-Major-3 |
| ghost-trust | HON-F4, SEC-m5, RD-m2 |
| ghost-providers | SEC-M1, SEC-m6, DUR-H1, DUR-M5, DUR-L1 |
| ghost-core | SEC-M2, SEC-M5, SEC-m7, RD-M2, RD-m1 |
| ghost-safety | SEC-M3, SEC-M4, SEC-m9, DUR-L1 |
| ghost-cli | RD-C1, RD-M1, RD-m3 |
| ghost-tools | SEC-m1, SEC-m8 |
| ghost-desktop | — |
| (dropped) saas | DRF-Major-1, DRF-Major-2, DRF-Minor-3 |

## Notes for Phase 5

- Update SPEC.md Bug 1 premise (stale — see [drift/Minor-4]) before porting; the real work is [drift/Major-3] (`ghost-hook` binary).
- The [durability/H1] flush policy (when/where the ledger persists) is a product decision affecting spend-guard guarantees — needs Inan's call.
- The Rust reader is field-compatible with Python state files per SPEC §4: it will ingest the **plaintext** `model.api_key` ([security/M1]) — plan a migration into the encrypted store.
- SPEC already names `SsrfGuard`, `redact_value`, `AuditLog` — wire them into default paths, not just configured ones ([security/M4], [security/M5], [security/M3]).
