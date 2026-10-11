# GHOST-Chimera (Rust)

Production-ready rebuild of GHOST-Chimera in Rust. See [SPEC.md](SPEC.md)
for the full Phase 0 architecture specification.

## Status

- **Phase 0**: done — SPEC.md (11 crates, module mapping, IPC design, provider strategy)
- **Phase 1**: done — `ghost-core`, `ghost-trust`, `ghost-stealth`
  - 48 tests passing, `cargo clippy -- -D warnings` clean, `cargo fmt --check` clean
  - Trust journals are byte-compatible with the Python implementation:
    Rust verifies Python-written journals and vice versa
    (`crates/ghost-trust/tests/py_journal_compat.rs`)

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
