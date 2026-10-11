# Security Policy

## Supported versions

GHOST-Chimera Rust is pre-1.0. Security fixes land on `main` and ship in the
next release; older pre-releases are not patched.

## Reporting a vulnerability

Please **do not open a public issue** for a security problem. Email the
maintainer at fernandogarzaaa@gmail.com, or use GitHub's private vulnerability
reporting from the repository's Security tab when it is enabled.

Include what you found, how to reproduce it, and the version or commit. You
should get an acknowledgement within a few days. Please give us reasonable
time to fix the issue before disclosing it publicly.

## Security model (summary)

- The console API is loopback-only unless a bearer token is configured; in
  tokenless mode it rejects non-loopback `Host` and `Origin` headers.
- Provider API keys are read from environment variables and are not written
  to disk by GHOST-Chimera. Secrets are redacted from journals and logs by
  `ghost-core`'s redaction layer.
- Known open security findings (for example SSRF protection and audit-log
  redaction for subsystems not yet ported) are tracked in
  [PHASE5_VERIFICATION.md](PHASE5_VERIFICATION.md).
