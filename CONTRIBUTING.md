# Contributing

Thanks for helping with the GHOST-Chimera Rust rebuild. Read [SPEC.md](SPEC.md)
first: it defines scope, and [PHASE5_VERIFICATION.md](PHASE5_VERIFICATION.md)
tracks which known gaps are fixed and which are still open.

## Before you open a pull request

Run the same checks CI enforces:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
python3 -m unittest discover -s .github/scripts -p 'test_*.py'
```

CI also runs:

- **supply-chain**: `cargo deny check` (licenses, bans, sources; see
  `deny.toml`) and `cargo audit`.
- **mutation-gate**: `cargo-mutants` on the Rust code your branch changes;
  at least 80% of generated mutants must be caught by tests. Timeouts and
  unviable mutants count as not caught. If the gate fails, add tests that
  kill the surviving mutants listed in the job log; do not lower the threshold.

## Ground rules

- **Honesty over green checks.** Code must never report success for work it
  did not do (no fabricated outcomes, no silent fallbacks). Prefer failing
  closed with a clear error.
- **Tests are hermetic.** Unit and integration tests must not touch the
  network or depend on API keys in the developer's environment. Use the
  offline kernel helpers (`build_offline_kernel`, `run_ask_with`).
- **Secrets never leave redaction.** Anything that can reach a log, journal,
  or API response goes through `ghost_core::redact`.
- **Keep docs true.** If a change alters behavior described in README.md,
  SPEC.md or PHASE5_VERIFICATION.md, update them in the same PR, and add an
  entry under `Unreleased` in CHANGELOG.md.
- Use [Conventional Commits](https://www.conventionalcommits.org/) style
  messages (`fix(core): …`, `ci: …`), and reference GAP ids where relevant.
