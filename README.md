# mcp-db-readonly

An MCP (Model Context Protocol) server that gives AI assistants **read-only** SQL access to your database — with enforcement baked into the server itself, not left to the AI's discretion.

## Why

When you let an AI assistant query your database, you want a hard guarantee that it cannot modify anything, regardless of what prompt it receives. This server provides two independent layers of enforcement:

1. **Session-level**: Connects with `SET SESSION CHARACTERISTICS AS TRANSACTION READ ONLY` — PostgreSQL itself will reject any write attempt.
2. **Keyword blocklist**: Blocks known write operations (`INSERT`, `UPDATE`, `DELETE`, `DROP`, `CREATE`, etc.) before the query even reaches the database, with a clear error message.

## Features

- **Enforced read-only** — two independent layers, not policy
- **PostgreSQL support** out of the box
- **Extensible** — `Database` trait makes adding MySQL, SQLite, etc. straightforward
- **TLS support** — `native-tls` by default for remote connections
- **ASCII table output** — clean, AI-readable query results
- **Configurable row limit** — default 500, user-adjustable up to 5000
- **Zero trust** — logs to stderr, stdout is pure MCP protocol

## Installation

### From source

```bash
git clone https://github.com/YOUR_USERNAME/mcp-db-readonly
cd mcp-db-readonly
cargo build --release
# Binary at: target/release/mcp-db-readonly
```

### Local dev (no TLS)

```bash
cargo build --no-default-features --release
```

## Usage

```
mcp-db-readonly --database-url <URL> [OPTIONS]

Options:
  --database-url <URL>    PostgreSQL connection URL [env: DATABASE_URL]
  --max-rows <N>          Default max rows per query [default: 500] [env: MCP_MAX_ROWS]
  --no-tls                Disable TLS (for local connections)
  -h, --help              Print help
  -V, --version           Print version
```

### Examples

```bash
# Local dev database
mcp-db-readonly --database-url "postgres://user:pass@localhost:5432/mydb" --no-tls

# Remote database with TLS (default)
mcp-db-readonly --database-url "postgres://user:pass@db.example.com:5432/mydb"

# Via environment variable
DATABASE_URL=postgres://user:pass@localhost/mydb mcp-db-readonly --no-tls
```

## Integrate with Claude Code

Add to your `.claude/settings.json` (or `~/.claude/settings.json` for all projects):

```json
{
  "mcpServers": {
    "db-readonly": {
      "command": "/path/to/mcp-db-readonly",
      "args": ["--database-url", "postgres://user:pass@localhost:5432/mydb", "--no-tls"],
      "env": {
        "RUST_LOG": "warn"
      }
    }
  }
}
```

Or using environment variables (recommended — keeps credentials out of config):

```json
{
  "mcpServers": {
    "db-readonly": {
      "command": "/path/to/mcp-db-readonly",
      "env": {
        "DATABASE_URL": "postgres://user:pass@localhost:5432/mydb",
        "MCP_MAX_ROWS": "500"
      }
    }
  }
}
```

Once configured, Claude will have access to a `sql_query` tool and can explore your schema and data.

## The `sql_query` tool

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `statement` | string | yes | The SQL statement to execute |
| `max_rows` | integer | no | Max rows (1–5000, default: server default) |

**Allowed statements**: `SELECT`, `EXPLAIN`, `EXPLAIN ANALYZE`, `SHOW`, `WITH ... SELECT`, and other read-only operations.

**Blocked statements**: `INSERT`, `UPDATE`, `DELETE`, `DROP`, `CREATE`, `ALTER`, `TRUNCATE`, `GRANT`, `REVOKE`, `COPY`, `VACUUM`, `ANALYZE` (standalone), `CALL`, `DO`, `EXEC`, `CLUSTER`, `REINDEX`, `REFRESH`, `IMPORT`.

### Example query results

```
+----+----------+------------------+
| id | username | email            |
+----+----------+------------------+
| 1  | alice    | alice@example.com|
| 2  | bob      | bob@example.com  |
+----+----------+------------------+
2 row(s)
```

## Extending to other databases

The `Database` trait in `src/db/traits.rs` is the extension point:

```rust
#[async_trait]
pub trait Database: Send + Sync {
    async fn execute_query(&self, sql: &str, max_rows: usize) -> Result<QueryResult, DbError>;
}
```

To add MySQL or SQLite:
1. Add the driver crate to `Cargo.toml` (optionally behind a feature flag)
2. Create `src/db/mysql.rs` or `src/db/sqlite.rs` implementing `Database`
3. Add a `--driver` CLI flag in `main.rs` to select the backend

The blocklist in `postgres.rs` is backend-specific and should be adapted for each driver's SQL dialect.

## Security model

| Threat | Mitigation |
|--------|-----------|
| Prompt injection attempts write operations | Session-level READ ONLY + keyword blocklist both reject it |
| Blocklist bypass via obfuscation | Session-level READ ONLY catches anything that slips through |
| Credentials in config | Use environment variables (`DATABASE_URL`) instead of inline args |
| Excessive data exposure | Row limit (default 500); use LIMIT in queries for large tables |

The read-only enforcement is **in the server**, not in the AI model's judgment. No prompt can override it.

## License

MIT
