# Security Policy

`peekdb` is designed to hold real database credentials and to be trusted with read-only
access to production data. Its core promise is that the enforcement layers described in the
README cannot be bypassed. A bug that breaks that promise — an enforcement bypass, a
credential leak (e.g. into the audit log or an error message), or a way to escalate beyond
read-only — is a security issue, not an ordinary bug.

## Reporting a vulnerability

**Please do not open a public GitHub issue for a security report.**

Use [GitHub's private vulnerability reporting](https://github.com/Harkirat155/mcp-db-readonly/security/advisories/new)
for this repository (Security tab → "Report a vulnerability"). This opens a private
conversation with the maintainer and lets a fix be prepared before public disclosure.

Please include:
- The affected version/commit.
- Which backend(s) are affected (Postgres / MongoDB / Redis / MCP server mode).
- A minimal reproduction — ideally a statement/command/config that shows the enforcement
  layer being bypassed, or credentials appearing somewhere they shouldn't (logs, error text,
  audit output).

## Scope

In scope:
- Any way to make a `query`/`mongo`/`redis` call (via the direct CLI or the `serve`/MCP path)
  perform a write, despite the documented allowlist/transaction-backstop/pipeline-stage
  checks.
- Any way for a connection string, password, or `${ENV_VAR}`-resolved secret to end up in
  the audit log, an error message, or other output.
- Config validation bypasses that let a literal password slip past `check_no_literal_password`
  without `allow_literal_password` being set.

Out of scope:
- Vulnerabilities in the underlying database engines themselves (Postgres/MongoDB/Redis) —
  report those upstream.
- Denial-of-service against a database you already have credentials for (statement timeouts
  are a courtesy, not a hard multi-tenant guarantee — this is a single-user local tool, not a
  shared service).

## Response

This is currently a single-maintainer project. There's no formal SLA, but security reports
get priority over feature work, and a fix or mitigation timeline will be communicated back
through the private advisory thread.
