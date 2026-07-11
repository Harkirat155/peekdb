## What this changes and why

## Testing

- [ ] `cargo fmt --all -- --check` and `cargo clippy --all-targets --all-features -- -D warnings` pass
- [ ] `cargo test --lib --bins` and `cargo test --test cli` pass (offline)
- [ ] If this touches `src/db/*`, `src/audit.rs`, or `src/server.rs`: ran the full
      container-backed integration suite (`docker compose -f docker-compose.test.yml up -d`
      then `cargo test --test integration_postgres --test integration_mongo --test
      integration_redis --test integration_mcp_serve -- --ignored --test-threads=1`) and it's
      green
- [ ] Added/updated tests covering the change (unit and, if applicable, a real-container
      integration test — see [CONTRIBUTING.md](CONTRIBUTING.md))

## Enforcement impact

If this changes what statements/operations/commands are allowed or blocked for any backend,
describe the change explicitly here — this is the part of the codebase where a subtle
regression matters most.
