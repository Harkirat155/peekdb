const TOOLS = [
  { name: "list_targets", use: "First call — names + backend" },
  { name: "list_objects", use: "Tables / collections / SCAN keys" },
  { name: "describe_object", use: "Columns, indexes, TYPE/TTL" },
  { name: "sample_object", use: "Shape inference (PG/Mongo)" },
  { name: "sql_query", use: "Read-only SQL → JSON by default" },
  { name: "mongo_query", use: "find/agg; nested $out blocked" },
  { name: "redis_query", use: "Allowlisted reads only" },
];

export function ForAgents() {
  return (
    <section className="section" id="for-agents" aria-labelledby="for-agents-title">
      <div className="container">
        <div className="section-head">
          <p className="eyebrow">for coding agents</p>
          <h2 className="section-title" id="for-agents-title">
            Built for tools that call tools
          </h2>
          <p className="section-lead">
            Prefer the machine brief — no JS required. Then wire MCP. Discovery tools first;
            query tools second. Writes never ship.
          </p>
        </div>

        <div className="agent-grid">
          <article className="card agent-card">
            <span className="label">llms.txt</span>
            <h3>Fetch the product brief</h3>
            <p>
              Plain-text install, MCP config, tool order, and security guarantees for agents that
              crawl docs.
            </p>
            <p className="agent-links">
              <a className="btn btn-primary" href="/peekdb/llms.txt">
                Open llms.txt
              </a>
              <a
                className="btn btn-ghost"
                href="https://github.com/Harkirat155/peekdb#readme"
                target="_blank"
                rel="noreferrer"
              >
                README
              </a>
            </p>
          </article>

          <article className="card agent-card">
            <span className="label">tool order</span>
            <h3>MCP surface</h3>
            <ol className="tool-list">
              {TOOLS.map((t) => (
                <li key={t.name}>
                  <code>{t.name}</code>
                  <span>{t.use}</span>
                </li>
              ))}
            </ol>
            <p className="muted-line">
              Resources: <code>peekdb://{"{target}"}/schema</code>,{" "}
              <code>…/tables|collections/{"{name}"}</code>
            </p>
          </article>
        </div>
      </div>
    </section>
  );
}
