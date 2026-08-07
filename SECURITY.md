# Security policy

podkit manages SSH connections to remote hosts, Podman sockets, secrets
(env vars, tokens), and TLS termination, so security bugs here have real
blast radius. Please report privately.

## Reporting a vulnerability

Use [GitHub Security Advisories](https://github.com/chikof/podkit/security/advisories/new)
for this repo ("Report a vulnerability" under the Security tab). Do not
open a public issue for a suspected vulnerability.

Include:

- Affected component (`server/`, `crates/core`, `crates/database`,
  `crates/runtime`, `crates/crypto`, `dashboard/`)
- Reproduction steps or PoC
- Impact as you understand it (e.g. auth bypass, secret disclosure, RCE
  via build/deploy path)

## Response

Best-effort acknowledgment within a few days. This is a small
single-maintainer project, so no formal SLA, but security reports are
prioritized over other work.

## Scope notes

- Secrets at rest go through `crates/crypto` (age encryption); plaintext
  secrets in the database or logs are a bug.
- Remote hosts are reached over SSH-tunneled `podman.sock`. Issues in
  `crates/runtime`'s tunnel/connection handling are in scope.
- Dependency vulnerabilities: Renovate keeps deps current; a report is
  still useful if a vulnerable version is in use before Renovate catches
  it.
