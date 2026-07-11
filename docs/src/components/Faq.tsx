const ITEMS = [
  {
    q: "Can an agent write to the database through peekdb?",
    a: "No. Multi-layer allowlists reject writes before they hit the wire. Postgres also uses session + transaction READ ONLY. Mongo blocks $out/$merge at any depth (including $facet/$lookup). Redis only allows listed read commands. There is no --write flag.",
  },
  {
    q: "Which MCP tools should an agent call first?",
    a: "list_targets → list_objects → describe_object / sample_object → sql_query | mongo_query | redis_query. Query tools return structured JSON by default; pass format: \"table\" for ASCII.",
  },
  {
    q: "Where do credentials live?",
    a: "In databases.toml via ${ENV_VAR} placeholders. Literal passwords are rejected at load time unless allow_literal_password = true for local dev. The audit log never stores connection strings.",
  },
  {
    q: "What about production safety controls?",
    a: "Optional per-target knobs: allow_tables / allow_collections, max_calls_per_minute, max_rows_per_minute, max_response_bytes, redact_columns / redact_patterns. Defaults keep full RO access within engine allowlists.",
  },
  {
    q: "Why not just use psql / mongosh / redis-cli?",
    a: "Those tools can write. They are not multi-backend MCP servers, have no structured agent discovery tools, and leave no append-only audit of every agent call.",
  },
  {
    q: "Which AI clients work?",
    a: "Any MCP client over stdio: Claude Code / Desktop, Cursor, Grok, and similar. Humans use the same binary via query, repl, explore, doctor, audit, and fav.",
  },
  {
    q: "Is this multi-tenant SaaS?",
    a: "No. peekdb runs on your machine with your credentials. Rate limits are process-local. Timeouts and row caps are single-user safety rails.",
  },
];

export function Faq() {
  return (
    <section className="section" id="faq" aria-labelledby="faq-title">
      <div className="container">
        <div className="section-head center">
          <p className="eyebrow">faq</p>
          <h2 className="section-title" id="faq-title">
            Straight answers
          </h2>
        </div>
        <div className="faq">
          {ITEMS.map((item) => (
            <details key={item.q}>
              <summary>{item.q}</summary>
              <p>{item.a}</p>
            </details>
          ))}
        </div>
      </div>
    </section>
  );
}
