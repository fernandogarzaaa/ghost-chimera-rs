# Releasing GHOST-Chimera Rust

How to cut a release of the Rust rebuild: the desktop Ghost Console
app plus the headless Rust binaries.

## Versioning

- SemVer, starting at **0.5.0**. (The Python line is `0.4.0-beta`; 1.0
  waits for the production-readiness checklist in SPEC.md section 7.)
- **Single source of truth, three places that must agree:**
  1. `[workspace.package] version` in `Cargo.toml`
  2. `version` in `desktop/package.json`
  3. The git tag, `v<version>` (e.g. `v0.5.0`)
- The release workflow's `check-version` job enforces this with
  `.github/scripts/check-version.py` and fails the release before any
  binary is built if they disagree.
- Runtime version is embedded via `env!("CARGO_PKG_VERSION")`; never
  hard-code a version string in Rust code or the Electron shell.

## Release process (checklist)

1. **Pick the commit.** Only release a commit that is green on `main`
   (CI: fmt, clippy, test, supply-chain, mutation gate).
2. **Open a release PR** against `main` containing:
   - Version bump in `Cargo.toml` (`[workspace.package] version`) AND
     `desktop/package.json` (`version`) to the new version.
   - `Cargo.lock` regenerated (`cargo build` to update it).
   - `CHANGELOG.md`: move entries from `[Unreleased]` into a new
     `## [X.Y.Z] - YYYY-MM-DD` section.
   - Anything else the release needs (migration notes, updater notes).
   
   What a release PR looks like (concrete example for 0.5.0):
   
   | File | Change |
   |---|---|
   | `Cargo.toml` | `version = "0.4.0"` -> `version = "0.5.0"` |
   | `Cargo.lock` | regenerated (`name = "ghost-core"` etc. version fields) |
   | `desktop/package.json` | `"version": "0.4.0"` -> `"version": "0.5.0"` |
   | `CHANGELOG.md` | new `## [0.5.0] - 2026-XX-XX` section |

3. **Merge only when CI is fully green**, including the mutation gate.
   (Standing rule: no PR merges without Inan's explicit merge call.)
4. **Tag the merge commit:** `git tag v0.5.0 && git push origin v0.5.0`.
5. **Watch the Release workflow** (`Actions > Release`):
   - `check-version` validates the three version sources.
   - `build-rust` compiles `--bins --workspace` for all four targets.
   - `electron` builds and publishes installers per OS to the GitHub
     Release (also writes `latest.yml`, the auto-update manifest).
   - `upload-cli` attaches zipped raw CLI binaries for headless use.
6. **Verify the release:** download each installer on its OS (or at
   minimum the Windows NSIS build on Inan's machine), fresh-install,
   launch, confirm the console opens and `ask` works. Check
   `latest.yml` exists on the release page (auto-update needs it).
7. **Announce** per whatever channel Inan uses for the release.

## Code signing

Signed builds are what make this "production-ready" instead of
"works on my machine". electron-builder signs automatically when the
secrets below exist; without them it builds **unsigned** and logs a
warning (SmartScreen / Gatekeeper will warn on install).

| Platform | Secret(s) | Cost / notes |
|---|---|---|
| Windows | `WINDOWS_CERT_PFX_BASE64` (base64 PFX), `WINDOWS_CERT_PASSWORD` | Standard cert ~$200-500/yr, EV ~$400-700/yr. Without a cert, SmartScreen warns; reputation builds over time as more users install. Self-signed is free but shows warnings: acceptable for alpha, not for production. |
| macOS sign | `MAC_CERT_P12_BASE64`, `MAC_CERT_PASSWORD` | Apple Developer Program $99/yr. Required for non-warning installs. |
| macOS notarize | `APPLE_ID`, `APPLE_APP_SPECIFIC_PASSWORD`, `APPLE_TEAM_ID` | Same $99/yr program; notarization is required or Gatekeeper blocks the dmg. |
| Linux | (none in CI) | GPG-sign the AppImage/deb offline; upload `.asc` alongside. Maintainer step, not automated yet. |

**Decision needed (SPEC Appendix A):** purchase certs now, or ship the
first alpha unsigned for Inan's own testing. The workflow supports
both; it does not fail when secrets are absent.

To add a secret: repo Settings > Secrets and variables > Actions.

## Auto-update wiring

- electron-builder `publish.provider: github` (already in
  `desktop/package.json`) plus `--publish always` in the release
  workflow writes `latest.yml` (and `-mac.yml`, `-linux.yml`) to the
  GitHub Release. **Do not rename or delete these assets**; the updater
  fetches them by name.
- The app side needs `electron-updater` wired into `main.js`
  (Phase 4 work): check on launch, prompt before installing, `stable`
  channel only for v1.0. No beta channel until asked.
- Rollback story: GitHub Releases keeps every version's assets, so a
  bad release is reverted by publishing a fixed version, never by
  deleting the release (deleting breaks clients mid-update).

## CI/CD overview

**`.github/workflows/ci.yml`** (every push to `main`, every PR):

| Job | What | Why separate |
|---|---|---|
| `fmt` | `cargo fmt --all -- --check` | instant signal |
| `clippy` | `cargo clippy --workspace --all-targets -- -D warnings` | instant signal |
| `test` | `cargo test --workspace --all-features` | correctness |
| `supply-chain` | `cargo deny check` + `cargo audit --deny warnings` | licenses, bans, advisories, CVEs |
| `mutants` | `.github/scripts/mutation-gate.py --fail-under 80` | 80% kill-rate gate on changed code (mirrors the Python `testteeth` policy). Separate job so a slow mutation run never blocks the fast jobs; a red gate is still a failing check. |

All jobs use `Swatinem/rust-cache` and cancel superseded runs on the
same ref (`concurrency.cancel-in-progress`).

**`.github/workflows/release.yml`** (on tags `v*`): version check,
four-target Rust builds, per-OS Electron packaging + publish,
CLI binary zips.

**Supply-chain configs:** `deny.toml` (crates.io only, no copyleft,
yanked/unsound = deny) and `.cargo/audit.toml` (RustSec advisories;
`ignore` entries require a dated comment + tracking issue).

**Mutation gate details** (`.github/scripts/mutation-gate.py`):
runs `cargo mutants --changed` (needs full git history, hence
`fetch-depth: 0`), parses the summary, cross-checks against
`mutants.json` when present, and **fails closed** on any parse
mismatch or zero mutants. Timeouts count as not-killed.

## What was verified where

Verified in the sandbox (this environment, no cargo, no network for
registry installs):

- All workflow YAML files parse (`pyyaml`).
- `deny.toml`, `rust-toolchain.toml` parse (`tomllib`); `.cargo/audit.toml` parses.
- `desktop/package.json` parses; `main.js` and `preload.js` pass
  `node --check`.
- `.github/scripts/check-version.py` and `mutation-gate.py` pass
  `python3 -m py_compile`; `check-version.py` was run against this repo
  and correctly passes/fails on matching/mismatched versions.

NOT verified here (needs CI or a maintainer machine):

- `cargo fmt` / `clippy` / `test` outcomes (no cargo in sandbox; the
  Rust gate runs in CI per standing rules).
- `cargo deny` / `cargo audit` against the real lockfile.
- `cargo mutants --changed` flag behavior and exact summary wording
  (the gate script fails closed on drift, but the first green CI run
  should be eyeballed).
- Electron packaging (`npm install` needs the Electron binary
  download; needs a real runner per OS).
- The `${arch}` macro in the macOS `extraResources` path: believed
  correct per electron-builder docs; verify on the first mac release.
- Windows/macOS signing and notarization (needs the secrets above).
