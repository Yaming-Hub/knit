# Security Policy

## Supported Versions

| Version | Supported |
|---------|-----------|
| 0.4.x   | ✅ Current |
| < 0.4   | ❌ No      |

## Reporting a Vulnerability

If you discover a security vulnerability, please report it responsibly:

1. **Do not** open a public GitHub issue.
2. Email the maintainer directly or use
   [GitHub's private vulnerability reporting](https://github.com/Yaming-Hub/knit/security/advisories/new).
3. Include a description of the vulnerability, steps to reproduce, and any
   potential impact.

You should receive a response within 7 days. We will work with you to
understand and address the issue before any public disclosure.

## Scope

Knit is a data generation tool. Security concerns include:

- **Blueprint injection** — Malicious blueprint files causing unintended
  behavior (e.g., path traversal in output paths).
- **WASM plugin safety** — Untrusted WASM plugins loaded via `--plugin`
  (sandboxed by Wasmtime, but still worth reporting issues).
- **Dependency vulnerabilities** — Issues in upstream crates that affect Knit.
