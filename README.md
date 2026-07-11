# peekdb

**[Website](https://harkirat155.github.io/peekdb/)** · **[Roadmap](ROADMAP.md)** · **[Releases](https://github.com/Harkirat155/peekdb/releases)**

A CLI (and MCP server) that gives you — or an AI coding agent — **read-only** access to
Postgres, MongoDB, and Redis, with the read-only guarantee enforced by the tool itself, not
left to good behavior.

## Why

When you or an AI assistant query a production or staging database, you want a hard
guarantee that nothing gets modified, regardless of what statement gets typed or what a
prompt talks an agent into trying. `peekdb` enforces that with independent layers per
backend, not a single "please be careful" check:

**PostgreSQL:**
1. **Transaction-level** — every query runs inside `BEGIN TRANSACTION READ ONLY … ROLLBACK`. PostgreSQL itself blocks `SET TRANSACTION READ WRITE` escalation inside a read-only transaction.
2. **Session-level** — the connection opens with `SET SESSION CHARACTERISTICS AS TRANSACTION READ ONLY` as a second layer.
3. **Statement allowlist** — a query is only sent to the database if it *starts with* `SELECT`, `WITH`, `EXPLAIN`, `SHOW`, `VALUES`, or `TABLE`; everything else (`INSERT`, `UPDATE`, `DELETE`, `DROP`, `CREATE`, `ALTER`, `GRANT`, `COPY`, `SET`, …) is rejected before it reaches the database. `EXPLAIN`'s own leading keyword doesn't exempt the statement it wraps — `EXPLAIN ANALYZE INSERT …` is still rejected.
4. **Single-statement enforcement** — interior `;` chains are rejected, preventing `SET …; INSERT …` patterns.

**MongoDB:**
1. **Operation allowlist** — only `find`, `aggregate`, `distinct`, `count`, `list_collections`, and `list_indexes` are accepted.
2. **Pipeline stage blocking** — `$out` and `$merge` stages (Mongo's equivalent of a write) are blocked in every aggregation pipeline before execution.

**Redis:**
1. **Command allowlist** — Redis has no read-only-transaction concept the way Postgres does, so this is the sole enforcement layer. Only read commands (`GET`/`MGET`, `HGET`/`HGETALL`, `SMEMBERS`, `ZRANGE`, `LRANGE`, the `SCAN` family, `TTL`, `EXISTS`, `KEYS`, etc.) and a few read-only subcommands (`CONFIG GET`, `CLUSTER INFO`) are accepted; everything else — `SET`/`DEL`/`EXPIRE`/`FLUSHALL`, scripting, transactions, pub/sub, `CONFIG SET`, `SHUTDOWN`, and anything not explicitly listed — is rejected by omission.

No prompt, flag, or client bug can talk `peekdb` into a write. Every call — allowed, blocked,
or failed — is recorded in an append-only audit log.

## Installation

### Prebuilt binaries

Download a tarball/zip for your platform from [GitHub Releases](https://github.com/Harkirat155/peekdb/releases)
— Linux, macOS, and Windows, x86_64 and aarch64.

### From source

```bash
git clone https://github.com/Harkirat155/peekdb
cd peekdb
cargo build --release
# Binary: target/release/peekdb
```

### PostgreSQL without TLS (local dev only)

`--no-default-features` disables the `native-tls` dependency for PostgreSQL and Redis TLS
connections. It does **not** affect MongoDB TLS, which uses a separate stack.

```bash
cargo build --no-default-features --release
```

## Quick start (direct CLI)

Scaffold a config (recommended) or copy the example:

```bash
peekdb init                                 # writes ~/.config/peekdb/databases.toml
# or:
cp databases.toml.example databases.toml   # gitignored — never committed
```

Config discovery (first match wins): `--config` → `$PEEKDB_CONFIG` → `./databases.toml` → `~/.config/peekdb/databases.toml`.

```toml
[[database]]
name   = "local"
url    = "postgres://postgres@localhost:5432/mydb"
no_tls = true

[[mongo]]
name = "mydb"
uri  = "mongodb://user:${MONGO_PASSWORD}@localhost:27017/mydb"

[[redis]]
name = "cache"
url  = "redis://localhost:6379"
```

Then query directly from the shell:

```bash
# Postgres
peekdb query --config databases.toml --db local --sql "SELECT * FROM users LIMIT 5"

# Mongo
peekdb mongo --config databases.toml --db mydb --collection users --operation find \
  --filter '{"status":"active"}'

# Redis
peekdb redis --config databases.toml --db cache HGETALL some-hash-key
```

`--db` can be omitted when only one database of that type is configured. `--json` (available
on all three subcommands) switches output to machine-readable JSON instead of a table or
pretty-printed values — useful when piping into `jq` or handing output to an AI agent.

A single-database shorthand also works for quick Postgres-only use without a config file:

```bash
peekdb query --database-url "postgres://user:pass@localhost:5432/mydb" --no-tls \
  --sql "SELECT 1"
```

## Features

- **Enforced read-only** — multiple independent layers per backend, not a "please be careful" policy
- **PostgreSQL + MongoDB + Redis** — three database types, one tool
- **Multi-database** — connect to several named instances from a single TOML config; pick by `--db`/`database` name
- **Environment-variable interpolation** — `${ENV_VAR}` in connection strings; literal passwords in the config file are rejected at load time
- **Audit logging** — an append-only JSON-Lines trail of every query/command, its outcome, and timing — identical whether invoked via the CLI or MCP
- **Configurable row/document/value limits** — sensible defaults, adjustable per call
- **Statement/query timeouts** — a runaway query gets killed server-side, not left to hang
- **TLS support** — `native-tls` for PostgreSQL and Redis remote connections (MongoDB uses its own TLS stack)
- **Extensible** — `Database`/`MongoDatabase`/`RedisDatabase` traits make adding a new backend straightforward

## Configuration reference

### PostgreSQL

```toml
[[database]]
name    = "local"
url     = "postgres://postgres@localhost:5432/mydb"
no_tls  = true

[[database]]
name    = "staging"
url     = "postgres://app_user:${STAGING_DB_PASSWORD}@staging-db.example.com:5432/app"
no_tls  = false

[[database]]
name    = "prod"
url     = "postgres://readonly_user:${PROD_DB_PASSWORD}@prod-db.example.com:5432/app"
no_tls  = false
```

### MongoDB

```toml
[[mongo]]
name = "mydb"
uri  = "mongodb://user:${MONGO_PASSWORD}@mongo.example.com:27017/mydb?authSource=admin"
```

The URI must include the database name in the path (`/mydb`). Replica set URIs
(`host1:port,host2:port`) are also supported.

### Redis

```toml
[[redis]]
name = "cache-local"
url  = "redis://localhost:6379"

[[redis]]
name = "cache-prod"
url  = "rediss://:${REDIS_PROD_PASSWORD}@redis-prod.example.com:6380"
```

Use `rediss://` (not `redis://`) for TLS. `${ENV_VAR}` placeholders in any connection string
are resolved at startup from the process environment — keep passwords out of the file
itself. A literal password in `databases.toml` is rejected at load time unless
`allow_literal_password = true` is set on that entry (for local-dev-only use).

## CLI reference

```
peekdb <COMMAND>

Commands:
  query         Run a read-only SQL statement against a configured PostgreSQL database
  mongo         Run a read-only operation against a configured MongoDB database
  redis         Run a read-only command against a configured Redis instance
  serve         Start the MCP (Model Context Protocol) server over stdio
  init          Create a starter databases.toml
  doctor        Probe every configured target
  config list   List targets with secrets masked
  mcp-snippet   Print a ready-to-paste MCP client config
  completions   Generate shell completions (bash/zsh/fish/…)
  audit         Filter the JSONL audit log (--target / --blocked / --outcome)
  repl          Interactive read-only SQL REPL (Postgres targets)
  explore       Menu-driven table browser (Postgres)
  fav           Named query favorites (list/show/save/rm)
```

### `init` / `doctor` / `config` / `mcp-snippet` / `audit` / interactive

```bash
peekdb init                          # ~/.config/peekdb/databases.toml
peekdb init --path ./databases.toml
peekdb doctor                        # SELECT 1 / list_collections / PING
peekdb config list                   # secrets masked
peekdb mcp-snippet                   # Claude / Cursor JSON
peekdb completions zsh > _peekdb
peekdb audit --blocked               # recent blocked attempts
peekdb audit --target staging --limit 50
peekdb repl --db staging             # interactive SQL (\\dt \\d \\sample \\fav)
peekdb explore --db staging          # list/describe/sample tables
peekdb fav save daily --sql "SELECT now()"
```

### `query` (PostgreSQL)

```
peekdb query [--config <PATH>] [--db <NAME>] [--sql <SQL>] [--max-rows <N>] \
  [--format table|vertical|csv|tsv|json] [--json]
```

`--sql` reads from stdin if omitted, so piping/heredocs work:

```bash
echo "SELECT * FROM widgets" | peekdb query --config databases.toml
```

### `mongo` (MongoDB)

```
peekdb mongo --config <PATH> [--db <NAME>] --operation <OP> [--collection <NAME>]
             [--filter <JSON>] [--projection <JSON>] [--sort <JSON>] [--pipeline <JSON>]
             [--field <NAME>] [--limit <N>] [--json]
```

`--operation` is one of `find`, `aggregate`, `distinct`, `count`, `list_collections`,
`list_indexes`.

### `redis` (Redis)

```
peekdb redis --config <PATH> [--db <NAME>] <COMMAND> [ARGS...] [--limit <N>] [--json]
```

```bash
peekdb redis --config databases.toml GET session:abc123
peekdb redis --config databases.toml HGET user:42 email
peekdb redis --config databases.toml SCAN 0 MATCH "session:*"
```

### `serve` (MCP server)

```
peekdb serve [--config <PATH>] [--database-url <URL>] [--no-tls] [--max-rows <N>]
```

Running `peekdb` with no subcommand (or `--config`/`--database-url` alone) defaults to
`serve`, for backwards compatibility with existing MCP client configs.

### Shared flags

All subcommands accept:

| Flag | Env var | Description |
|------|---------|-------------|
| `--audit-log <PATH>` | `MCP_AUDIT_LOG` | Path to the audit log (JSON Lines). Defaults to `<config-dir>/audit.jsonl`. |
| `--no-audit-log` | — | Disable audit logging entirely for this run. |

`serve` and `query` also accept `--max-rows <N>` (env `MCP_MAX_ROWS`, default 500).

## MCP server mode

`peekdb` also runs as an MCP (Model Context Protocol) server over stdio, for AI clients that
talk MCP tool-calling instead of shelling out to a CLI directly.

### Integrate with Claude Code

Add to `~/.claude/mcp.json` (or the Claude Desktop / Cursor equivalent):

```json
{
  "mcpServers": {
    "db-readonly": {
      "command": "/path/to/peekdb",
      "args": ["--config", "/path/to/databases.toml"],
      "env": {
        "STAGING_DB_PASSWORD": "...",
        "MONGO_PASSWORD": "...",
        "REDIS_PROD_PASSWORD": "...",
        "RUST_LOG": "warn"
      }
    }
  }
}
```

Once configured, the AI has discovery tools first (`list_targets`, `list_objects`,
`describe_object`, `sample_object`), then `sql_query` / `mongo_query` / `redis_query`.
Query tools return **structured JSON by default** (columns/rows + meta); pass
`"format": "table"` for ASCII. Schema is also exposed as MCP resources
(`peekdb://{target}/schema`, `peekdb://{target}/tables/{name}`).

### `list_targets` / `list_objects` / `describe_object` / `sample_object`

| Tool | Purpose |
|------|---------|
| `list_targets` | Configured names + backend (`postgres` / `mongo` / `redis`) |
| `list_objects` | Tables, collections, or Redis `SCAN` (pattern optional) |
| `describe_object` | Columns/indexes (PG), inferred fields+indexes (Mongo), TYPE/TTL (Redis) |
| `sample_object` | Sample rows/docs (PG/Mongo only) |

### `sql_query` (PostgreSQL)

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `database` | string | yes (multi-DB) | Name matching a `[[database]]` entry in the config |
| `statement` | string | yes | SQL statement to execute (one statement; no `;` chains) |
| `max_rows` | integer | no | Max rows returned (1–5000; default: server default) |

**Allowed**: statements starting with `SELECT`, `WITH`, `EXPLAIN`, `SHOW`, `VALUES`, `TABLE`.
Everything else is rejected before it reaches the database.

**Example output:**

```
+----+----------+-------------------+
| id | username | email             |
+----+----------+-------------------+
| 1  | alice    | alice@example.com |
| 2  | bob      | bob@example.com   |
+----+----------+-------------------+
2 row(s)
```

### `mongo_query` (MongoDB)

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `database` | string | yes | Name matching a `[[mongo]]` entry in the config |
| `operation` | string | yes | One of: `find`, `aggregate`, `distinct`, `count`, `list_collections`, `list_indexes` |
| `collection` | string | required for most ops | Target collection (not required for `list_collections`) |
| `filter` | string | no | JSON filter document, e.g. `{"status": "active"}` |
| `projection` | string | no | JSON projection document, e.g. `{"name": 1, "_id": 0}` |
| `sort` | string | no | JSON sort document, e.g. `{"createdAt": -1}` |
| `pipeline` | string | no | JSON array of aggregation stages (required for `aggregate`) |
| `field` | string | no | Field name (required for `distinct`) |
| `limit` | integer | no | Max documents returned (1–1000; default: 100) |

**Blocked pipeline stages**: `$out`, `$merge` (both write to collections).

**Example output:**

```
[1] {"_id": {"$oid": "64a1f2..."}, "name": "Alice", "status": "active"}
[2] {"_id": {"$oid": "64a1f3..."}, "name": "Bob", "status": "active"}
2 document(s)
```

Documents are serialized as MongoDB Extended JSON (`{"$oid": "..."}` for ObjectIds,
`{"$date": ...}` for dates, etc.).

### `redis_query` (Redis)

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `database` | string | yes (multi-instance) | Name matching a `[[redis]]` entry in the config |
| `command` | string | yes | Redis command, e.g. `GET`, `HGETALL`, `SMEMBERS` (read-only commands only) |
| `args` | array of strings | no | Arguments to the command, in order |
| `limit` | integer | no | Max items displayed for list/set/hash-like results (1–1000; default: 100) |

**Example output:**

```
{"email": "alice@example.com", "plan": "pro"}
```

## Optional safety controls (Phase 2)

Per target in `databases.toml` (all optional; defaults keep full access within RO rules):

```toml
[[database]]
name = "staging"
url = "postgres://ro:${STAGING_DB_PASSWORD}@db/app"
allow_tables = ["users", "orders"]
max_calls_per_minute = 60
max_rows_per_minute = 5000
max_response_bytes = 1048576
redact_columns = ["email"]
redact_patterns = ["*ssn*"]
```

- **Allowlists** apply on SQL table refs (FROM/JOIN/TABLE), Mongo collections, Redis keys, and filter discovery lists.  
- **Rate limits** are process-local sliding windows (calls recorded per attempt).  
- **Redaction** rewrites matching columns/fields to `[REDACTED]` and sets audit `redacted: true`.  
- **Max response bytes** truncates large payloads or errors if a single row is still too large.

## Security model

| Threat | PostgreSQL mitigation | MongoDB mitigation | Redis mitigation |
|--------|----------------------|-------------------|-------------------|
| Prompt injection write attempt | Leading-statement allowlist rejects before DB; `BEGIN TRANSACTION READ ONLY` rejects at DB regardless | Operation allowlist: only read operations accepted | Command allowlist: only read commands accepted |
| `SET TRANSACTION READ WRITE` escalation | `SET` isn't an allowed leading statement; PostgreSQL also blocks it inside a read-only transaction | N/A | N/A |
| Aggregation pipeline writing to a collection | N/A | `$out` and `$merge` stages are detected and blocked before execution | N/A |
| Statement chaining (`SET …; INSERT …`) | Single-statement enforcement: interior `;` is rejected | N/A | N/A (one command per call by construction) |
| Credential exposure | `${ENV_VAR}` interpolation; literal passwords in `databases.toml` are rejected at load time; the file itself is gitignored | Same | Same |
| Excessive data exposure | Row limit (default 500, max 5000) | Document limit (default 100, max 1000) | Display limit (default 100, max 1000) |
| Slow/runaway query | Server-side `statement_timeout` (default 30s) | `maxTimeMS` (default 30s) | N/A (single-command calls) |

## Extending to other databases

Three traits are the extension points:

```rust
// For SQL databases
#[async_trait]
pub trait Database: Send + Sync {
    async fn execute_query(&self, sql: &str, max_rows: usize) -> Result<QueryResult, DbError>;
}

// For MongoDB-style document databases
#[async_trait]
pub trait MongoDatabase: Send + Sync {
    async fn execute_mongo(&self, query: MongoQuery) -> Result<MongoResult, DbError>;
}

// For single-command key/value stores
#[async_trait]
pub trait RedisDatabase: Send + Sync {
    async fn execute_redis(&self, query: RedisQuery) -> Result<RedisResult, DbError>;
}
```

To add a new backend: implement the appropriate trait (`src/db/postgres.rs`,
`src/db/mongo.rs`, or `src/db/redis.rs` as a reference), wire a new config section in
`src/config.rs` and `src/lib.rs`, and expose the tool in `src/server.rs`.

## License

MIT — see [LICENSE](LICENSE).
