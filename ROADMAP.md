# peekdb Roadmap

**North star:** Make peekdb the default way developers and coding agents safely look at databases — **read-only by construction**, agent-first UX, without expanding the product story to “more engines.”

> Other DB CLIs help you type SQL. Other MCPs let agents query.  
> **peekdb is the one that cannot write — by construction — and still feels great for both.**

Site: [harkirat155.github.io/peekdb](https://harkirat155.github.io/peekdb/) · Repo: [Harkirat155/peekdb](https://github.com/Harkirat155/peekdb)

---

## What we have today

| Surface | Entry | Capabilities |
|---------|-------|--------------|
| MCP (stdio) | `peekdb serve` | `sql_query`, `mongo_query`, `redis_query` |
| CLI one-shot | `query` / `mongo` / `redis` | Pretty tables or `--json` |

**Differentiator — multi-layer enforcement:**

- **Postgres:** statement allowlist + single-statement + session RO + transaction RO + timeouts + row caps  
- **Mongo:** operation allowlist + `$out`/`$merge` blocked  
- **Redis:** deny-by-default command allowlist  
- **Everywhere:** env password interpolation, no literal secrets by default, append-only audit log  

**Experience gaps:** no schema discovery tools, MCP returns ASCII tables as text, manual TOML only, no REPL, no rate limits / table allowlists / PII redaction.

---

## Principles

1. **Agent-first** — Claude, Cursor, Grok, and friends are the primary users; humans get an excellent second surface.  
2. **Controls over connectors** — deepen safety and UX on PG/Mongo/Redis before adding engines.  
3. **Dogfood gate** — each phase must make the maintainer want daily use before marketing it.  
4. **No write mode. Ever.**  

---

## Phase 0 — Product surface & install

*Goal: zero-friction first run.*

| Deliverable | Notes |
|-------------|--------|
| `peekdb init` | Scaffold `~/.config/peekdb/databases.toml` + env export examples |
| Config discovery | `./databases.toml` → `$PEEKDB_CONFIG` → `~/.config/peekdb/databases.toml` |
| `peekdb doctor` | Probe every named target; green/red matrix |
| `peekdb config list` | Named targets with secrets masked |
| `peekdb mcp-snippet` | Print ready Claude / Cursor / Grok MCP JSON |
| Shell completions | clap generate for zsh / bash / fish |
| Install story | Releases, `cargo install`, optional Homebrew later |

---

## Phase 1 — Agent DX (highest leverage)

*Goal: agents stop guessing; results are machine-native.*

### New MCP tools (only for configured backends)

| Tool | Purpose |
|------|---------|
| `list_targets` | Named DBs + backend type (always first) |
| `list_tables` / `list_collections` / capped `SCAN` | Structure discovery |
| `describe_table` / `describe_collection` | Columns, types, indexes |
| `sample_rows` / `sample_docs` | Shape inference |
| Existing query tools | Keep; add structured JSON result mode |

### MCP result contract

```json
{
  "columns": ["id", "email"],
  "rows": [[1, "a@x.com"]],
  "meta": {
    "target": "staging",
    "backend": "postgres",
    "row_count": 1,
    "truncated": false,
    "duration_ms": 12
  }
}
```

- MCP default = **JSON**; optional `format: "table"`.  
- CLI stays pretty-by-default + `--json`.

### MCP Resources

- `peekdb://{target}/schema`  
- `peekdb://{target}/tables/{name}`  

Passive schema context without tool spam. Shorter tool descriptions; point agents at `list_*` first.

---

## Phase 2 — Safety for real staging / prod

| Control | Design |
|---------|--------|
| Rate limit | Per-target calls/min + rows/min |
| Table / collection allowlists | Optional per config entry |
| PII / column redaction | Configured columns or patterns; audit marks `redacted` |
| Max response bytes | Cap agent context blowups |
| Nested Mongo write stages | Detect `$out`/`$merge` inside `$facet` / `$lookup` |
| `peekdb audit` | Filter / tail JSONL for humans |
| Profiles | `env = "prod"` tags; refuse prod unless opted in |

---

## Phase 3 — Human delight

| Item | Notes |
|------|--------|
| `peekdb repl` | Multi-line, history, `\d` / `\dt` / `\c` |
| Output modes | table · vertical · csv · tsv · json |
| Favorites | Named snippets in config |
| Query history | Successful queries for re-run |
| `peekdb explore` (TUI) | Optional ratatui explorer **after** schema APIs exist |

---

## Phase 4 — Platform polish (ongoing)

- crates.io + Homebrew / Scoop notes  
- MCP protocol conformance (SDK only if hand-rolled hurts)  
- Agent recipes (Claude / Cursor / Grok configs)  
- Security diagrams on the site  

---

## Differentiating bets (category-defining)

1. **`preview: true`** — plan / approx row count without full run  
2. **Sticky session** — last target + schema cache in MCP process  
3. **Auto EXPLAIN meta** on slow Postgres queries  
4. **Client identity** in audit (`mcp` / `cli` / `repl` + optional client name)  
5. **`peekdb prove`** — battery of blocked writes → green checklist  
6. **Local schema cache** (`.peekdb/schema-cache.json`, TTL)  

---

## Explicitly out of scope

- Headline roadmap for MySQL / SQLite / ClickHouse / etc. (traits stay open)  
- Multi-tenant cloud SaaS  
- Any write path  
- Premature MCP stack rewrite  

---

## Status

| Phase | Status |
|-------|--------|
| 0 Install / doctor / config UX | **Done** (`init`, `doctor`, `config list`, `mcp-snippet`, completions, config discovery) |
| 1 Agent schema tools + JSON | Planned |
| 2 Safety controls | Planned |
| 3 REPL / TUI | Planned |
| 4 Platform | Ongoing |
| Landing page | Shipped (`docs/` → GitHub Pages) → [site](https://harkirat155.github.io/peekdb/) |

Contributions welcome — see [CONTRIBUTING.md](CONTRIBUTING.md). Prefer opening an issue against a phase item before a large PR.
