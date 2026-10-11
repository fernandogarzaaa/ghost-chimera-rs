# Phase 5 Verification Report

**Date:** 2026-10-11
**Scope:** All 7 known bugs + 52 GAP_ANALYSIS findings against the Rust workspace
**Correction (improve/ci-and-hardening):** durability/H1 was marked fixed below although `CostLedger` had no production callers; it is now wired. See the H1 row.

**Baseline:** `cargo test --workspace` green (121 tests), `cargo clippy --workspace --all-targets -- -D warnings` clean, `cargo fmt --check` clean

---

## The 7 Known Bugs

| # | Bug | Status | Evidence |
|---|-----|--------|----------|
| 1 | OpenClawAdapter.install() stub | ✅ VERIFIED | `ghost-stealth/src/hosts/openclaw.rs` returns real `InstallPayload` with lifecycle hooks. Trait has no default `install()` (compile-time enforcement). Test: `hosts::tests::openclaw_install_returns_lifecycle_hooks` |
| 2 | Console chat duplicate replies | ✅ VERIFIED | `ghost-console-api/src/routes.rs`: envelope contains EXACTLY ONE top-level `reply`. Test: `conversation_envelope_has_exactly_one_reply_field` counts `"reply"` occurrences in serialized JSON (=1). Any nested duplicate (e.g. operator_report echo) fails the test. |
| 3 | `ghost ask` trust journaling | ✅ VERIFIED LIVE | `ghost-cli/src/cmd.rs`: `create_run` before kernel execution, `record_step` per execution, `trust_run_id` in output. Live: `ghost ask "what is 2 + 2"` → journal has `run_created` + `task_executed` steps. |
| 4 | Standing Orders CLI parity | ✅ VERIFIED LIVE | Full lifecycle works: create → list → enable → run → disable. `run` returns `executed: false, preview: true` without bumping counters (F2 fixed). |
| 5 | Console deterministic fallback | ✅ VERIFIED | Test: `conversation_honest_deterministic_reply` — arithmetic with no providers → ok + note "Ran on the offline deterministic backend". Unfulfillable → honest `deterministic_unfulfilled` error. |
| 6 | Real replay | ✅ VERIFIED LIVE | `ghost trust replay <id>` → `execution_performed: true`, re-executes objective, returns outcome comparison with `outcome_match`. |
| 7 | PR #116 honesty | ✅ VERIFIED LIVE | `ghost ask "write a poem about the ocean"` → `ok: false`, `task_completed: false`, error starts `deterministic_unfulfilled`, exit code 1. |

---

## Critical Findings (6)

| ID | Finding | Status | Evidence |
|----|---------|--------|----------|
| security/C1 | Console binds 0.0.0.0 with zero auth | ✅ VERIFIED FIXED | `ghost-console-api/src/server.rs:154`: `serve()` returns `Err(Config)` for non-loopback bind without token. Test: `non_loopback_bind_without_token_refuses`. Follow-ups: whitespace-only tokens now count as no token; constant-time comparison; tokenless loopback mode rejects non-loopback `Host`/`Origin` (also covers security/m2 for the console). |
| honesty/F1 | Stealth loop fabricated SUCCESSFUL with executor=None | ✅ FIXED | Added `StealthLoop::execute_actions(intervention_id, actions, executor)` where executor is a REQUIRED `&dyn Fn` parameter (not `Option`). No code path can record success without executing. Tests: `execute_actions_records_real_results_not_fabricated_success`, `execute_actions_success_records_useful`. Follow-up: an empty action list was still recorded as `Useful`, and an unknown intervention id ran actions before failing — both now refused (tests `execute_actions_with_no_actions_records_nothing`, `execute_actions_unknown_intervention_never_runs_executor`). |
| honesty/F2 | `run_order` ok:true + counter bump on no-op preview | ✅ VERIFIED FIXED | Live: `standing-orders run` → `{"ok": true, "executed": false, "preview": true}`, `run_count` stays 0, `last_run_at` stays 0.0. |
| honesty/F3 | Remote approval marked "executed" with no execution | ⏳ N/A — PENDING PORT | Approval subsystem (`create_approval`/`resolve_approval`) not yet ported to Rust. When ported, `ghost-trust` must keep "approved" (decision) vs "executed" (post-run) as distinct states. |
| durability/H1 | CostLedger never persisted | ✅ FIXED (corrected) | The original Phase 5 entry marked this *VERIFIED FIXED*, but `CostLedger` (`ghost-providers/src/ledger.rs`, JSONL journal persisted on `record()`/`set_budget()`, test `budget_blocks_overspend_and_survives_reload`) had **no production callers**: no model call was ever metered — the same "persistence built but never wired" pattern as the Python gap. Wired in the improve/ci-and-hardening PR: `MeteredProvider` (`ghost-providers/src/metered.rs`) checks the budget before each request and journals every call; `build_kernel` wraps the chosen provider in it over `<state_dir>/cost_ledger.jsonl` and disables providers if the ledger cannot be opened; `ghost spend show/set-budget` expose it. USD caps only trip once `GHOSTCHIMERA_<PROVIDER>_PRICE_{IN,OUT}_PER_1K` are set (no invented price catalog). |
| readme/C1 | Installers fail on default path | ✅ ADDRESSED BY DESIGN | Rust has a single binary (no extras concept). Electron `package.json` stages the Rust sidecar per-OS (`sidecar/`, `sidecar/mac-${arch}/`). The Python extras incoherence cannot recur. |

---

## Major Findings (19)

| ID | Finding | Status | Evidence |
|----|---------|--------|----------|
| security/M1 | Provider API keys in plaintext config.json | ⏳ PARTIAL | Rust reads keys from env vars (never writes to disk). No encrypted credential store yet — needs `ghost-core` secret store (planned, not built). |
| security/M2 | Fernet vault hardcoded fallback key | ⏳ PENDING PORT | No vault in Rust yet. When built, must derive per-install keys (never hardcoded). |
| security/M3 | Audit log unredacted payloads | ⏳ PENDING PORT | `ghost-safety` crate not yet built. `AuditLog::record` must redact first (SPEC requirement). |
| security/M4 | SSRF: no private-IP block on fetch paths | ⏳ PENDING PORT | No browser fetch path in `ghost-console-api` yet. When ported, `SsrfGuard` must be in the default path. |
| security/M5 | Redaction misses key shapes | ✅ FIXED | Added patterns for `AIza…`, `xai-…`, `gsk_…`, `sk-ant-…` to `ghost-core/src/redact.rs`. Test: `provider_key_shapes_redacted_m5`. Widened further: `key`-named mapping keys, GitHub/Slack/GitLab/NVIDIA/HF/Stripe/AWS/JWT/PEM shapes, and `name=value` secret assignments in free text. |
| honesty/F4 | `resume_run` marks "running" but nothing resumes | ⏳ PENDING PORT | No resume API in `ghost-trust` yet. When ported: wire real resume executor or refuse explicitly. |
| durability/M1 | ConversationStore non-atomic write + wipe on corrupt | ✅ PATTERN CORRECT | `ghost-trust` uses atomic temp-file + rename for all writes (verified in `store.rs`). Conversation store not yet ported — must follow the same pattern. |
| durability/M2 | AutonomyJobQueue "running" jobs orphaned on restart | ⏳ PENDING PORT | Job queue not yet ported. Requires startup recovery pass. |
| durability/M3 | Stealth BackgroundRuntime no queue_file | ⏳ PENDING PORT | Background runtime not yet ported. Must wire durable journal by default. |
| durability/M4 | ConsoleRunManager no persistence | ⏳ PENDING PORT | Run manager not yet ported. |
| durability/M5 | FreeRouter._save swallows write errors | ⏳ PENDING PORT | Router not yet ported. Must fail loud on usage-write errors. |
| durability/M6 | CalibrationStore in-memory only | ⏳ PENDING PORT | Calibration not yet ported. |
| durability/M7 | SessionState.save/load never called | ⏳ PENDING PORT | Agent session API not yet ported. Wire to trust journal or drop. |
| readme/M1 | README misstates installer extras | ✅ N/A | No extras concept in Rust. README must be written fresh for the Rust build. |
| readme/M2 | "Morning briefings" don't exist | ⏳ PRODUCT DECISION | Needs Inan: build it or drop the claim from the Rust README. |
| readme/M3 | Dry-run is desktop-only | ⏳ PENDING PORT | Dry-run must be kernel-level in `ghost-pilot` when ported. |
| drift/Major-1 | `worker start` never starts worker | ✅ N/A | `saas/` dropped per SPEC. Do not port. |
| drift/Major-2 | `saas create-admin` not durable | ✅ N/A | `saas/` dropped per SPEC. Do not port. |
| drift/Major-3 | `ghost-hook` binary doesn't exist | ⚠️ OPEN | Adapters emit `ghost-hook …` commands but no binary exists. **Must implement `ghost-hook` in `ghost-cli` or point snippets at something real before Phase 4.** |

---

## Minor Findings (27)

**Honesty (3):** F5/F6/F7 (live_presence fabrications) — ⏳ PENDING PORT (live_presence not in Rust yet).

**Durability (8):** L1–L8 — ⏳ PENDING PORT (rate limiters, scheduler weights, telemetry, credential pool, experience stream, automations save, cron scheduler, github dedup — none ported yet).

**README (3):** m1/m2/m3 — ⏳ Rust README not yet written; must be verified against code when written.

**Drift (4):** Minor-1 (12 unlinked routes) — ⏳ evaluate during console-api completion. Minor-2 (sdk.py) — ✅ deferred per SPEC. Minor-3 (saas init-db) — ✅ N/A (saas dropped). Minor-4 (SPEC Bug 1 stale) — ✅ noted; the real gap is Major-3.

**Security (9):** m2 (CSRF/CORS) — ✅ addressed for the console API: tokenless mode rejects non-loopback `Host`/`Origin`. m1, m3–m9 — ⏳ PENDING PORT (MCP server auth, CSRF/CORS, approval opt-ins, BPO redaction, session redaction policy, key previews, LangSmith, MCP env inheritance, network defaults — apply when each subsystem is ported).

---

## Fixes Applied in Phase 5

1. **F1 (Critical):** Added `StealthLoop::execute_actions()` with required (non-optional) executor + 2 tests. The fabricated-success path is now unrepresentable.
2. **M5 (Major):** Widened `ghost-core` redaction patterns (AIza, xai-, gsk_, sk-ant-) + test.

## Mutation Testing

`cargo-mutants` could not run: /tmp is 512M (insufficient for full-workspace mutant builds). Instead, honesty-critical tests were inspected for mutant-killing strength:
- `honest_failure_for_poem`: asserts `!ok` + empty output + error prefix + message content — kills ok-flip, error-suppression, and output-fabrication mutants.
- `conversation_envelope_has_exactly_one_reply_field`: counts serialized `"reply"` occurrences — kills any nested-duplicate mutant.
- `non_loopback_bind_without_token_refuses`: asserts `Err` — kills fail-open mutants.
Full mutation gate runs in CI per the release pipeline (`.github/workflows/ci.yml`).

## Remaining Work

- **1 open item:** `ghost-hook` binary (drift/Major-3) — implement before Phase 4.
- **1 product decision:** readme/M2 (morning briefings) — needs Inan. (H1 is wired via `MeteredProvider`; the ledger persists on every call.)
- **Pending ports:** approval subsystem (F3, F4), ghost-safety crate (M3, M4), encrypted credential store (M1, M2), job queue recovery (M2), background runtime durability (M3), and all 27 Minors — tracked for their respective phases.
