# Changelog

All notable changes to this project are documented here.

## [0.1.0] - 2026-07-11

### Added

- Multi-database support with lazy connections
- Add MongoDB support and prepare for GitHub release
- Security hardening — config validation, audit logging, SQL allowlist, timeouts
- Add Redis backend with command allowlist, CLI/MCP parity, audit coverage

### Changed

- Finish CLI subcommand split, extract src/lib.rs, fix two bugs
- Rename package to peekdb, keep mcp-db-readonly as compat binary

### Documentation

- Add OSS contributor docs, security policy, templates, and changelog tooling
- Add cargo package --list check to the release hard gate

### Miscellaneous

- Update GitHub URLs to Harkirat155 account
- Gitignore local debug wrapper script and audit log files
- Add release workflow (prebuilt binaries) and manual release checklist
- Drop mcp-db-readonly compat binary now that the GitHub repo is renamed peekdb

### Other

- Initial commit: mcp-db-readonly MCP server

### Testing

- Add unit + CLI + real-container integration test suite, CI pipeline

