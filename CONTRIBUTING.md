# Contributing to peekdb

## Dev setup

1. Rust (stable) via [rustup](https://rustup.rs).
2. Docker, for the integration test containers.
3. Clone and build:
   ```bash
   git clone https://github.com/Harkirat155/peekdb
   cd peekdb
   cargo build
   ```

## Running the tests

There are three layers. Run all of them before opening a PR — CI runs all three too.

### 1. Unit tests (offline, no DB needed)

```bash
cargo test --lib --bins
```

### 2. CLI black-box tests (offline, no DB needed)

Spawns the built binary and asserts on stdout/stderr/exit code.

```bash
cargo test --test cli
```

### 3. Integration tests (real containers — the actual release gate)

Every enforcement mechanism and every backend is exercised against a **real running
instance**, not mocks. This is deliberate: two real bugs (a Redis RESP2 flat-array decoding
bug, and an audit-log misclassification bug) were only caught by this layer, not by unit
tests. Treat it as required, not optional, for any change touching `src/db/*`, `src/audit.rs`,
or `src/server.rs`.

```bash
docker compose -f docker-compose.test.yml up -d
cargo test --test integration_postgres --test integration_mongo --test integration_redis \
  --test integration_mcp_serve -- --ignored --test-threads=1
docker compose -f docker-compose.test.yml down
```

`--test-threads=1` is required: the tests share and reseed one table/collection/keyspace per
backend, so they can't run concurrently. `docker-compose.test.yml` uses non-default ports
(`55432`/`57017`/`56379`) so it won't collide with any Postgres/Mongo/Redis you already have
running locally. This is the same file CI's `services:` block mirrors — keep them in sync if
you change image versions.

### Lint

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
```

## Adding a new backend

The three traits in `src/db/traits.rs` (`Database`, `MongoDatabase`, `RedisDatabase`) are the
extension points. To add a new one (e.g. a fourth database type):

1. Add a config struct to `src/config.rs` (mirror `RedisConfig`), including it in `Config`
   behind a `#[serde(default)]` `Vec`, and extend `check_no_literal_password` /
   duplicate-name validation to cover it.
2. Add a trait + result/query types to `src/db/traits.rs` if the shape doesn't fit an
   existing trait (a single-command store looks like `RedisDatabase`; a document/aggregation
   store looks like `MongoDatabase`; a SQL-like store looks like `Database`).
3. Implement the trait in a new `src/db/<name>.rs`, enforcing read-only via **allowlist**
   (not a blocklist — see the Postgres/Mongo/Redis modules for why: a blocklist that scans
   whole statement text has false-positive and false-negative failure modes a leading-token
   or operation allowlist doesn't).
4. Wire a new subcommand in `src/lib.rs` (`Command` enum, `<Name>Args`, `build_<name>_databases`,
   `pick_<name>`, `run_<name>`) — **and add the new subcommand name to the `SUBCOMMANDS` list
   in `inject_serve_default`**, or the legacy zero-args-defaults-to-serve shim silently breaks
   for it.
5. Wrap the backend in an audited type in `src/audit.rs` (mirror `AuditedRedis`) and construct
   it at both the CLI and `run_serve` call sites in `src/lib.rs` — both entry points must get
   audit coverage, not just one.
6. Add MCP tool registration + dispatch in `src/server.rs` (`handle_tools_list`,
   `handle_tools_call`).
7. Write unit tests for the allowlist logic, CLI black-box tests in `tests/cli.rs`, and an
   exhaustive `tests/integration_<name>.rs` against a real container added to both
   `docker-compose.test.yml` and `.github/workflows/ci.yml`'s `services:` block. Extend
   `tests/integration_mcp_serve.rs` to cover the new backend's `tools/call` path too.

## Versioning

SemVer, starting from `0.1.0`. `1.0.0` is reserved for once all three backends have been
stable for a while, the CLI flag surface has held for a full `0.x` release without breaking
changes, and Phase 2's security hardening (config validation, audit logging, allowlists,
timeouts) has been battle-tested — not before. Until then, expect `0.x` bumps for
breaking changes to CLI flags, config shape, or MCP tool schemas.

## Changelog

Commit messages should be reasonably descriptive of *why*, not just *what* — they feed the
changelog. Changelog generation uses [git-cliff](https://git-cliff.org):

```bash
cargo install git-cliff   # once
git cliff -o CHANGELOG.md
```

Run this (and commit the updated `CHANGELOG.md`) when cutting a release tag, not on every PR.

## Reporting security issues

See [SECURITY.md](SECURITY.md) — please don't open a public issue for a credential-handling
or enforcement-bypass bug.
