# GHOST-Chimera v0.5.0 — Release Notes (DRAFT)

**First Rust release.** This is a ground-up rebuild of GHOST-Chimera
(Python `0.4.0-beta`, ~78k lines) in Rust, with every known bug from the
Python line fixed during the port — several made *unrepresentable* at the
type level rather than merely patched.

## What's in the box

- **Single binary.** `ghost` replaces the Python package, its extras
  matrix, and the interpreter dependency. `cargo install` or the platform
  installer is the whole install story.
- **Desktop app.** The Ghost Console ships as a real Electron application
  (Windows NSIS installer first, macOS dmg and Linux AppImage/deb to
  follow) with the Rust backend bundled as a sidecar. No browser, no
  `localhost:8766` to remember.
- **Trust Runtime.** Hash-chained JSONL run journals, approval lifecycle,
  eval baselines — field-compatible with the Python journals, so your
  existing `~/.ghostchimera` state migrates on first run (copied, never
  moved) into the OS-appropriate data directory.
- **Stealth Loop.** Ambient event fabric, world state, intervention
  lifecycle, and six host adapters (OpenClaw, OpenCode, Claude Code,
  Codex, Gemini, Hermes).
- **`ghost-hook` binary.** The lifecycle-hook commands referenced by every
  adapter install payload (`ghost-hook openclaw-ingest`, `prompt`, `mcp`,
  …) now actually exist: each invocation runs the payload through a real
  `StealthLoop` (event-sourced across processes via a durable journal)
  and answers JSON on stdout. Includes a minimal MCP-over-stdio server
  for the Codex/OpenCode adapters.
- **Providers.** Trait-based provider system; one `reqwest` OpenAI-compatible
  client covers the 24 compatible providers, plus Anthropic and local
  (Ollama/LMStudio) bridges.
- **Honesty, enforced.** The deterministic offline backend fulfills what it
  can (arithmetic, file listing) and returns machine-readable
  `deterministic_unfulfilled` failures otherwise — exit code 1, never a
  fabricated success. The stealth loop cannot record success without
  executing (the executor is a required parameter, not an `Option`).
- **Real replay.** `ghost trust replay <run-id>` re-executes the journaled
  objective and reports an outcome comparison, not a policy preview.
- **Standing Orders CLI.** `ghost standing-orders list|create|enable|disable|run`
  — full parity with the console.
- **Crash reports.** Panics write a TOML report to `<state_dir>/crashes/`
  (on-disk only, no telemetry) with a friendly message.
- **Fail-closed networking.** The console refuses non-loopback binds
  without a console token.

## vs the Python version

| Area | Python 0.4.0-beta | Rust 0.5.0 |
|------|-------------------|------------|
| Install | pip + extras matrix | Single binary / installer |
| Console | Browser at localhost | Electron desktop app |
| State dir | `~/.ghostchimera` | OS data dir (+ auto-migration) |
| Trust journals | JSONL, compatible | JSONL, field-compatible |
| `ghost-hook` | Referenced but missing | Implemented |
| Stealth executor=None | Fabricated SUCCESSFUL | Unrepresentable (type-level) |
| Cost ledger | In-memory, lost on restart | Durable JSONL journal |
| Console auth | Open on 0.0.0.0 by default | Fail-closed without token |

## Known limitations

- **No encrypted credential store yet.** Provider keys are read from
  environment variables and never written to disk; the encrypted vault
  (and its per-install key derivation) is planned, not built.
- **No `ghost-safety` crate yet.** Audit-log redaction-first and the
  SSRF guard land with their respective subsystem ports.
- **Approval subsystem not yet ported.** `create/resolve_approval`
  keep "approved" vs "executed" as distinct states when it lands.
- **Background runtime, job queue, scheduler, calibration** — durability
  wiring is specified (GAP_ANALYSIS.md) and lands with each port.
- **Morning briefings** (a Python README claim) — product decision
  pending: build it or drop the claim.
- **Signing.** Release builds are unsigned until signing certificates are
  provisioned (Windows ~$200–700/yr, Apple $99/yr + notarization).
  Expect SmartScreen / Gatekeeper warnings on 0.5.0.

## Upgrade / migration notes

1. Install the `ghost` binary or the desktop app.
2. On first run, if `~/.ghostchimera` exists and the new state dir is
   empty, it is **copied** (never moved) into the OS-appropriate location:
   - Windows: `%LOCALAPPDATA%\ghost-chimera`
   - macOS: `~/Library/Application Support/ghost-chimera`
   - Linux: `~/.local/share/ghost-chimera`
3. Trust journals are read-compatible; run history, approvals, and eval
   baselines carry over.
4. `GHOSTCHIMERA_STATE_DIR` still overrides everything (operator/CI
   escape hatch).
5. The Python package is frozen and receives no further updates; the
   Rust line is the living one.

## Verification

- `cargo test --workspace`: all green
- `cargo clippy --workspace --all-targets -- -D warnings`: clean
- `cargo fmt --check`: clean
- Test count and mutation-gate results: see CI run for the release tag.
