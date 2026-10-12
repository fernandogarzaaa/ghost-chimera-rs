# Changelog

All notable changes to the GHOST-Chimera Rust rebuild are documented here.
Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/);
versioning follows [SemVer](https://semver.org/) starting at 0.5.0
(the Python line is 0.4.0-beta; 1.0 waits for the production-readiness
checklist in SPEC.md section 7 to be fully green).

## [Unreleased]

## [0.5.1] - 2026-10-12

### Security
- Console: whitespace-only tokens count as no token; tokens are compared in
  constant time. Tokenless (loopback-only) mode rejects non-loopback `Host`
  headers (DNS rebinding) and non-loopback or `null` `Origin` headers
  (cross-site POSTs) (GAP security/C1 follow-ups, security/m2).
- Redaction (GAP security/M5): mapping keys containing `key`
  (`private_key`, `accessKey`, `x-api-key`; known non-secret ids such as
  `idempotency_key`/`public_key` allow-listed for journal compatibility), more
  token shapes (all GitHub prefixes, Slack `xox?-`, GitLab `glpat-`, NVIDIA
  `nvapi-`, Hugging Face `hf_`, Stripe, AWS key ids, JWTs, PEM private keys),
  and `name=value` / `name: value` secret assignments in free text.

### Added
- `MeteredProvider` wraps every model provider: budget checked before the
  request, every successful call journaled to `cost_ledger.jsonl`, reply
  withheld if the journal write fails; prices from
  `GHOSTCHIMERA_<PROVIDER>_PRICE_{IN,OUT}_PER_1K` (GAP durability/H1 now
  actually wired). Providers are disabled if the ledger cannot be opened.
- `ghost spend show` and `ghost spend set-budget --provider --usd`.
- `CostLedger::snapshot()` / `journal_path()`; `ModelProvider::model()`.
- LICENSE (MIT), CONTRIBUTING.md, SECURITY.md; README architecture diagram,
  CI badge, usage.
- Unit tests for the mutation-gate result parser, run in CI.

### Fixed
- Console UI returned 404 in released binaries: static assets were read from
  the build machine's `CARGO_MANIFEST_DIR`; they are now embedded with
  `include_bytes!` and served by exact name only.
- StealthLoop (GAP honesty/F1): an empty action list no longer records a
  `Useful` outcome, and an unknown intervention id is rejected before any
  action runs.
- `CostLedger::set_budget` refuses NaN/infinite/negative caps (a NaN cap
  disabled the guard); `record` refuses non-finite/negative prices.
- Mutation gate: parses cargo-mutants 27.x `outcomes.json` correctly (per-
  scenario `summary` field, results under `<dir>/mutants.out`, baseline
  excluded); no longer rejects all-caught runs. Unviable and timed-out mutants
  still count as not killed.
- `deny.toml` migrated to the cargo-deny >= 0.18 schema.
- Release `upload-cli-binaries` failed with `zip error: Nothing to do!`
  (`zip -j archive .` adds no files); it now zips the staged binaries and
  restores their executable bit.

### Changed
- reqwest uses rustls only (`default-features = false`): no OpenSSL or
  `pkg-config` needed to build. reqwest and tempfile hoisted into
  `[workspace.dependencies]`; tower bumped to 0.5 to drop a duplicate.
- Tests are hermetic: CLI and console tests use an offline kernel, env-mutating
  integration tests are serialized and clear every provider key, and the
  loopback test client bypasses HTTP proxies.

## [0.5.0] - TBD

First Rust release. See SPEC.md for the phase plan.
