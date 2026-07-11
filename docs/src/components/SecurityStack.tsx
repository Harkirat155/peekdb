const LAYERS = [
  {
    n: "01",
    title: "Statement / command allowlist",
    body: "SELECT-family SQL only; Mongo read ops; Redis deny-by-default reads. Nested $out/$merge blocked anywhere in a pipeline.",
  },
  {
    n: "02",
    title: "Session + transaction RO",
    body: "Postgres: SET SESSION … READ ONLY and BEGIN TRANSACTION READ ONLY … ROLLBACK on every call.",
  },
  {
    n: "03",
    title: "Optional product controls",
    body: "Table/collection allowlists, rate limits, max response bytes, column/PII redaction — config-driven, off by default.",
  },
  {
    n: "04",
    title: "Append-only audit",
    body: "Every call (allowed, blocked, failed) lands in JSONL. Independent of RUST_LOG. peekdb audit filters the trail.",
  },
];

export function SecurityStack() {
  return (
    <section className="section" id="security" aria-labelledby="security-title">
      <div className="container">
        <div className="section-head center">
          <p className="eyebrow">security model</p>
          <h2 className="section-title" id="security-title">
            Layers, not vibes
          </h2>
          <p className="section-lead">
            Prompt injection can talk an agent into trying a write. It cannot talk peekdb into
            executing one.
          </p>
        </div>
        <div className="stack">
          {LAYERS.map((layer) => (
            <div className="stack-layer" key={layer.n}>
              <div className="stack-badge">{layer.n}</div>
              <div>
                <strong>{layer.title}</strong>
                <span>{layer.body}</span>
              </div>
            </div>
          ))}
        </div>
      </div>
    </section>
  );
}
