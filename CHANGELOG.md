# Changelog

All notable changes to this project are documented here.

## [Unreleased]

### Added

- Multi-database support with lazy connections
- Add MongoDB support and prepare for GitHub release
- Security hardening — config validation, audit logging, SQL allowlist, timeouts
- Add Redis backend with command allowlist, CLI/MCP parity, audit coverage

### Changed

- Finish CLI subcommand split, extract src/lib.rs, fix two bugs
- Rename package to peekdb, keep mcp-db-readonly as compat binary

### Miscellaneous

- Update GitHub URLs to Harkirat155 account
- Gitignore local debug wrapper script and audit log files

### Other

- Initial commit: mcp-db-readonly MCP server

### Testing

- Add unit + CLI + real-container integration test suite, CI pipeline

