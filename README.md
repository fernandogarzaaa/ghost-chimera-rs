# GHOST-Chimera (Rust)

GHOST-Chimera rebuilt as a Rust workspace with a local-first CLI, console API,
provider adapters, trust journaling, and an Electron desktop shell. The rebuild
is under active development; a green unit-test job is not by itself a production
readiness claim. See [SPEC.md](SPEC.md) for the architecture and intended scope.

## Status

- **Workspace:** seven Rust crates (`ghost-core`, `ghost-trust`, `ghost-stealth`,
  `ghost-providers`, `ghost-pilot`, `ghost-console-api`, and `ghost-cli`).
- **Desktop:** Electron shell that launches the Rust `ghost` sidecar.
- **Verification:** CI runs formatting, Clippy, workspace tests, dependency
  policy checks, mutation testing, and desktop JavaScript syntax checks.
  Check the latest [CI run](https://github.com/fernandogarzaaa/ghost-chimera-rs/actions/workflows/ci.yml)
  for the current result rather than relying on a static test count.
- **Compatibility:** trust journals have cross-language fixtures in
  `crates/ghost-trust/tests/py_journal_compat.rs`.
- **Readiness boundary:** provider behavior, security controls, persistence
  recovery, and packaged installers still require the automated and manual
  verification described in [RELEASING.md](RELEASING.md) and
  [desktop/TESTING.md](desktop/TESTING.md).

## Crates

| Crate | Responsibility |
|---|---|
| `ghost-core` | Config, error types, secret redaction, stable ids, Python-compatible JSON |
| `ghost-trust` | `TrustRuntimeStore`: hash-chained JSONL run journals |
| `ghost-stealth` | Event bus, StealthLoop, 6 host adapters (real `install()` payloads) |

## Build

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

## Key implementation notes

- **Hash-chain compatibility**: `ghost-core::canonical_json` is byte-identical
  to CPython's `json.dumps(sort_keys=True, separators=(",", ":"))`, including
  `ensure_ascii` escaping. Verified against CPython 3.12 test vectors.
- **`float_roundtrip`**: serde_json's default float parser is not correctly
  rounded, which silently breaks hash verification for timestamps. The
  workspace enables `float_roundtrip` — do not remove it.
- **No stub `install()`**: the `HostAdapter` trait has no default `install()`,
  so the OpenClaw stub (Python bug #1) is unrepresentable at compile time.
