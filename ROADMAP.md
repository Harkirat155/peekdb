# peekdb Roadmap

**North star:** Make peekdb the default way developers and coding agents safely look at databases — **read-only by construction**, agent-first UX, without expanding the product story to “more engines.”

> Other DB CLIs help you type SQL. Other MCPs let agents query.  
> **peekdb is the one that cannot write — by construction — and still feels great for both.**

Site: [harkirat155.github.io/peekdb](https://harkirat155.github.io/peekdb/) · Repo: [Harkirat155/peekdb](https://github.com/Harkirat155/peekdb)

---

## What we have today

| Surface | Entry | Capabilities |
|---------|-------|--------------|
| MCP (stdio) | `peekdb serve` | Discovery tools + `sql_query` / `mongo_query` / `redis_query` (JSON by default) + Resources; structured `{error,code,layer,hint}` on failures |
| CLI | `query` / `mongo` / `redis` / `init` / `doctor` / `config` / `mcp-snippet` / `completions` / `audit` / `repl` / `explore` / `fav` | Pretty tables or `--format` / `--json`; config discovery |

**Differentiator — multi-layer enforcement:**

- **Postgres:** statement allowlist + embedded-write scan (DML inside `WITH`/CTEs) + single-statement + session RO + transaction RO + timeouts + row caps  
- **Mongo:** operation allowlist + recursive `$out`/`$merge` block (any depth under `$facet` / `$lookup`)  
- **Redis:** deny-by-default command allowlist  
- **Everywhere:** env password interpolation, no literal secrets by default, append-only audit log  
- **Optional (config):** table/collection allowlists, rate limits, max response bytes, column/PII redaction  

---

## Principles

1. **Agent-first** — Claude, Cursor, Grok, and friends are the primary users; humans get an excellent second surface.  
2. **Controls over connectors** — deepen safety and UX on PG/Mongo/Redis before adding engines.  
3. **Dogfood gate** — each phase must make the maintainer want daily use before marketing it.  
4. **No write mode. Ever.**  
5. **Discovery and safety ride `execute_*`** — no parallel unguarded connection API.  
6. **Read-only *is* the product** — no extra “prod profile gates”; the same enforcement applies wherever credentials point.

---

## Phase status

| Phase | Status |
|-------|--------|
| 0 Install / doctor / config UX | **Done** |
| 1 Agent schema tools + JSON | **Done** |
| 2 Safety controls | **Done** (allowlists, rate limits, redaction, max bytes, nested Mongo RO, `audit` CLI) |
| 3 REPL / explore / formats / favorites | **Done** |
| 4 Platform + safety depth | **In progress** |
| Landing page | Shipped (`docs/` → GitHub Pages) |

### Learnings from Phase 0–3

1. **Discovery should ride `execute_*`** — list/describe/sample call the same audited RO path as queries.  
2. **Fewer tools beat per-backend clones** — `list_objects` / `describe_object` / `sample_object` resolve backend from unique target names.  
3. **JSON default is a deliberate MCP break** — agents parse structured envelopes; humans use CLI tables or `format: "table"`.  
4. **Identifier allowlist for sample SQL** — only safe `[A-Za-z_][A-Za-z0-9_]*` identifiers.  
5. **Redis SCAN is one cursor step** — pattern refinement, not full keyspace dump.  
6. **`list_targets` is pure config** — no audit line.  
7. **Rate limits are process-local** — each peekdb process has its own window counters (not multi-process).  
8. **Nested write stages need a full BSON walk** — top-level stage keys alone miss `$facet` / `$lookup` sub-pipelines.  
9. **Record rate-limit calls on attempt** — otherwise failed connects never consume quota and agents can still hammer.  
10. **REPL is postgres-first** — multi-backend REPL is possible later; one interactive surface beats a half-baked multi-backend prompt.  
11. **Explore reuses schema helpers** — no second connection stack.  
12. **Leading-keyword allowlist alone is not enough for `WITH`** — data-modifying CTEs need an embedded-write scan (outside strings/comments) *plus* the RO transaction backstop.

---

## Phase 4 — Platform + safety depth (current)

Ship distribution and deepen the RO story without changing the product promise.

| Item | Status | Notes |
|------|--------|--------|
| Client-side DML-in-CTE / embedded write rejection | **Done** (this track) | `DbError::EmbeddedWrite`; audit as `blocked` |
| Structured MCP errors (`code` / `layer` / `hint`) | **Done** (this track) | Agents distinguish allowlist vs rate limit vs DB errors |
| ROADMAP accuracy | **Done** (this track) | Phase 3 no longer listed as open |
| crates.io publish | Ready | Metadata + install docs; run manual `cargo publish` (see RELEASING.md) |
| Install polish (`cargo install` / `binstall` / git / releases) | **Done** | README + site + llms.txt |
| Homebrew tap | Deferred | Wait for external demand |
| Schema cache (TTL) for discovery tools | **Done** | `PEEKDB_SCHEMA_CACHE_TTL_SECS` (default 60) |
| Result pagination / cursors | **Done** | `page_size` + `cursor` / `meta.next_cursor`; `PEEKDB_RESULT_CACHE_TTL_SECS` |
| Structured client MCP errors | **Done** | Same envelope as DbError (`unknown_target`, `missing_argument`, …) |
| Multi-backend REPL | Open | Only if dogfooded daily |
| Full TUI `explore` | Open | Menu-driven is enough until it isn’t |
| Shared rate limits across processes | Open | Optional; document process-local clearly for now |

### Explicitly not Phase 4

- Extra prod “are you sure?” gates — RO by construction already *is* the safety story  
- New database engines as a headline  
- Any write path  

---

## Phase 2 controls (shipped)

Optional TOML keys per `[[database]]` / `[[mongo]]` / `[[redis]]` (see `databases.toml.example`):

| Key | Effect |
|-----|--------|
| `allow_tables` / `allow_collections` | Restrict objects; empty = unrestricted |
| `max_calls_per_minute` | Sliding-window call cap |
| `max_rows_per_minute` | Sliding-window rows/docs cap |
| `max_response_bytes` | Truncate/error oversized payloads |
| `redact_columns` / `redact_patterns` | Mask matching fields in results; audit `redacted` |

```bash
peekdb audit --target staging --blocked
peekdb audit --path ./audit.jsonl --outcome ok --limit 20
```

---

## Explicitly out of scope

- Headline roadmap for more engines  
- Multi-tenant cloud SaaS  
- Any write path  

Contributions welcome — see [CONTRIBUTING.md](CONTRIBUTING.md).
