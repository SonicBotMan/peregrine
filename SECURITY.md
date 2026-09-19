# Security Policy

Peregrine is a download manager: it fetches files from the internet,
writes them to disk, executes nothing it downloads, and exposes a
loopback-only control API. This document explains what is in scope and
how to report vulnerabilities.

## Supported versions

| Version | Supported |
| --- | --- |
| latest `2.0.0-alpha.x` release / `main` | ✅ |

Pre-1.0 alpha users are expected to run the latest build; only the
current line receives fixes.

## Reporting a vulnerability

**Preferred:** use GitHub's private vulnerability reporting —
`Security` tab → *Report a vulnerability* on
https://github.com/SonicBotMan/peregrine.

**Alternative:** email **523034406@qq.com** with `[peregrine-security]`
in the subject.

Please include: affected version/commit, a minimal reproduction, and
your assessment of impact. You will get an acknowledgment within **72
hours** and a status update at least every 7 days until a fix or
mitigation ships.

## Scope notes (what the threat model actually is)

- The daemon binds **loopback only** (`127.0.0.1` TCP + a `0700` UDS).
  Remote exposure is a misconfiguration, but loopback CSRF/DNS-rebinding
  hardening (host guard + CORS allowlist) is in scope and tested.
- The TCP API surface optionally accepts a Bearer token
  (`--auth-token`); the bearer-auth path and its `/health` exemption
  are security-relevant code.
- The MCP server exposes download control to AI agents — protocol-level
  issues there (prompt-injected destructive operations, path escapes)
  are in scope and taken seriously.
- Anything requiring local code execution as the user is out of scope.

## Known accepted risks (documented, not bugs)

- The glib `VariantStrIter` unsoundness advisory
  (GHSA-wrw7-89jp-8q8g) is tracked in `.github/dependabot.yml` — the
  affected API is not called by this tree and no glib 0.18.x fix
  exists; re-evaluated when tauri ships a gtk-rs 0.20 stack.
