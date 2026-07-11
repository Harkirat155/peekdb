# mcp-db-readonly

An MCP (Model Context Protocol) server that gives AI assistants **read-only** access to your databases — with enforcement baked into the server, not left to the AI's discretion.

Supports **PostgreSQL**, **MongoDB**, and **Redis**.

## Why

When you let an AI assistant query a database, you want a hard guarantee that it cannot modify anything, regardless of what prompt it receives. This server provides multiple independent enforcement layers per database type:

**PostgreSQL:**
1. **Transaction-level** — every query runs inside `BEGIN TRANSACTION READ ONLY … ROLLBACK`. PostgreSQL blocks `SET TRANSACTION READ WRITE` escalation inside a read-only transaction.
2. **Session-level** — the connection opens with `SET SESSION CHARACTERISTICS AS TRANSACTION READ ONLY` as a second layer.
3. **Statement allowlist** — a query is only sent to the database if it *starts with* `SELECT`, `WITH`, `EXPLAIN`, `SHOW`, `VALUES`, or `TABLE`; everything else (`INSERT`, `UPDATE`, `DELETE`, `DROP`, `CREATE`, `ALTER`, `GRANT`, `COPY`, `SET`, …) is rejected before it reaches the database. `EXPLAIN`'s own leading keyword doesn't exempt the statement it wraps — `EXPLAIN ANALYZE INSERT …` is still rejected.
4. **Single-statement enforcement** — interior `;` chains are rejected, preventing `SET …; INSERT …` patterns.

**MongoDB:**
1. **Operation whitelist** — only `find`, `aggregate`, `distinct`, `count`, `list_collections`, and `list_indexes` are accepted.
2. **Pipeline stage blocking** — `$out` and `$merge` stages (the MongoDB equivalent of a write) are blocked in every aggregation pipeline before execution.

**Redis:**
1. **Command allowlist** — Redis has no read-only-transaction concept the way Postgres does, so this is the sole enforcement layer. Only read commands (`GET`/`MGET`, `HGET`/`HGETALL`, `SMEMBERS`, `ZRANGE`, `LRANGE`, `SCAN` family, `TTL`, `EXISTS`, `KEYS`, etc.) and a few read-only subcommands (`CONFIG GET`, `CLUSTER INFO`) are accepted; everything else — `SET`/`DEL`/`EXPIRE`/`FLUSHALL`, scripting, transactions, pub/sub, `CONFIG SET`, `SHUTDOWN`, and anything not explicitly listed — is rejected by omission.

The read-only enforcement is **in the server**, not in the AI model's judgment. No prompt can override it.

## Features

- **Enforced read-only** — multiple independent layers, not a "please be careful" policy
- **PostgreSQL + MongoDB + Redis** — three database types supported in one tool
- **Multi-database** — connect to several named instances from a single TOML config; the AI (or you) picks by name
- **Environment-variable interpolation** — `${ENV_VAR}` in connection strings; literal passwords in the config file are rejected at load time
- **Audit logging** — an append-only JSON-Lines trail of every query/command, its outcome, and timing — covering both the direct-CLI and MCP paths identically
- **Configurable row/document/value limits** — sensible defaults, adjustable per call
- **ASCII table output** (SQL) and JSON output (Mongo/Redis) — clean, AI-readable results
- **TLS support** — `native-tls` for PostgreSQL and Redis remote connections (MongoDB uses its own TLS stack)
- **Extensible** — `Database`/`MongoDatabase`/`RedisDatabase` traits make adding new backends straightforward

## Installation

### From source

```bash
git clone https://github.com/Harkirat155/mcp-db-readonly
cd mcp-db-readonly
cargo build --release
# Binary: target/release/mcp-db-readonly
```

### PostgreSQL without TLS (local dev only)

`--no-default-features` disables the `native-tls` dependency for PostgreSQL connections. It does **not** affect MongoDB TLS, which uses a separate stack.

```bash
cargo build --no-default-features --release
```

## Configuration

Copy the example and fill in your connection details:

```bash
cp databases.toml.example databases.toml   # gitignored — never committed
```

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

The URI must include the database name in the path (`/mydb`). The server extracts it at startup. Replica set URIs (`host1:port,host2:port`) are also supported.

`${ENV_VAR}` placeholders are resolved at startup from the process environment — keep passwords out of the file.

### Starting the server

```bash
STAGING_DB_PASSWORD=secret MONGO_PASSWORD=secret2 \
  mcp-db-readonly --config databases.toml
```

### Single-database shorthand (PostgreSQL only)

```bash
# Local dev
mcp-db-readonly --database-url "postgres://user:pass@localhost:5432/mydb" --no-tls

# Remote with TLS
DATABASE_URL=postgres://user:pass@db.example.com:5432/mydb mcp-db-readonly
```

### CLI reference

```
mcp-db-readonly [OPTIONS]

Options:
  --config <PATH>           TOML config file listing named databases
  --database-url <URL>      Single PostgreSQL URL [env: DATABASE_URL]
  --database-name <NAME>    Name for --database-url connection [default: default]
  --no-tls                  Disable native-tls for PostgreSQL connections
  --max-rows <N>            Default max rows per query [default: 500] [env: MCP_MAX_ROWS]
  -h, --help                Print help
  -V, --version             Print version
```

`--config` and `--database-url` are mutually exclusive.

## Integrate with Claude Code

Add to `~/.claude/mcp.json` (or the Claude Desktop / Cursor equivalent):

### Single PostgreSQL database

```json
{
  "mcpServers": {
    "db-readonly": {
      "command": "/path/to/mcp-db-readonly",
      "env": {
        "DATABASE_URL": "postgres://user:pass@localhost:5432/mydb",
        "RUST_LOG": "warn"
      },
      "args": ["--no-tls"]
    }
  }
}
```

### Multiple databases (PostgreSQL + MongoDB)

```json
{
  "mcpServers": {
    "db-readonly": {
      "command": "/path/to/mcp-db-readonly",
      "args": ["--config", "/path/to/databases.toml"],
      "env": {
        "STAGING_DB_PASSWORD": "...",
        "MONGO_PASSWORD": "...",
        "RUST_LOG": "warn"
      }
    }
  }
}
```

Once configured, the AI will have access to the `sql_query` and `mongo_query` tools and can explore schema and data across all configured databases.

## Tool reference

### `sql_query` (PostgreSQL)

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `database` | string | yes (multi-DB) | Name matching a `[[database]]` entry in the config |
| `statement` | string | yes | SQL statement to execute (one statement; no `;` chains) |
| `max_rows` | integer | no | Max rows returned (1–5000; default: server default) |

**Allowed**: `SELECT`, `EXPLAIN`, `EXPLAIN ANALYZE`, `SHOW`, `WITH … SELECT`, and other read-only operations.

**Blocked keywords**: `INSERT`, `UPDATE`, `DELETE`, `MERGE`, `DROP`, `CREATE`, `ALTER`, `TRUNCATE`, `GRANT`, `REVOKE`, `COPY`, `VACUUM`, `ANALYZE` (standalone), `CALL`, `DO`, `EXEC`, `CLUSTER`, `REINDEX`, `REFRESH`, `IMPORT`, `SET`, `COMMIT`, `ROLLBACK`, `SAVEPOINT`, `RELEASE`.

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

---

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

**Allowed operations:**

| Operation | What it does |
|-----------|-------------|
| `find` | Query documents with optional filter, projection, sort, limit |
| `aggregate` | Run an aggregation pipeline (`$out` and `$merge` stages are blocked) |
| `distinct` | Return distinct values for a field |
| `count` | Count documents matching an optional filter |
| `list_collections` | List all collection names in the database |
| `list_indexes` | List indexes for a collection |

**Blocked pipeline stages**: `$out`, `$merge` (both write to collections).

**Example output:**

```
[1] {"_id": {"$oid": "64a1f2..."}, "name": "Alice", "status": "active"}
[2] {"_id": {"$oid": "64a1f3..."}, "name": "Bob", "status": "active"}
2 document(s)
```

Documents are serialized as MongoDB Extended JSON (`{"$oid": "..."}` for ObjectIds, `{"$date": ...}` for dates, etc.).

## Security model

| Threat | PostgreSQL mitigation | MongoDB mitigation | Redis mitigation |
|--------|----------------------|-------------------|-------------------|
| Prompt injection write attempt | Leading-statement allowlist rejects before DB; `BEGIN TRANSACTION READ ONLY` rejects at DB regardless | Operation whitelist: only read operations accepted | Command allowlist: only read commands accepted |
| `SET TRANSACTION READ WRITE` escalation | `SET` isn't an allowed leading statement; PostgreSQL also blocks it inside a read-only transaction | N/A | N/A |
| Aggregation pipeline writing to a collection | N/A | `$out` and `$merge` stages are detected and blocked before execution | N/A |
| Statement chaining (`SET …; INSERT …`) | Single-statement enforcement: interior `;` is rejected | N/A | N/A (one command per call by construction) |
| Credential exposure | `${ENV_VAR}` interpolation; literal passwords in `databases.toml` are rejected at load time; the file itself is gitignored | Same | Same |
| Excessive data exposure | Row limit (default 500, max 5000) | Document limit (default 100, max 1000) | Display limit (default 100, max 1000) |
| Slow/runaway query | Server-side `statement_timeout` (default 30s) | `maxTimeMS` (default 30s) | N/A (single-command calls) |

## Extending to other databases

Two traits in `src/db/traits.rs` are the extension points:

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
```

To add a new backend, implement the appropriate trait (see `src/db/postgres.rs` or `src/db/mongo.rs` for reference), wire a new config section in `src/config.rs` and `src/main.rs`, and expose the tool in `src/server.rs`.

## License

MIT — see [LICENSE](LICENSE).
