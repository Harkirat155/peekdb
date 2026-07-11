const LAYERS = [
  {
    n: "01",
    title: "Statement / command allowlist",
    body: "Only SELECT-family SQL, read Mongo ops, and read Redis commands ever leave the process.",
  },
  {
    n: "02",
    title: "Session read-only",
    body: "Postgres opens with SET SESSION CHARACTERISTICS AS TRANSACTION READ ONLY.",
  },
  {
    n: "03",
    title: "Transaction backstop",
    body: "Every query runs inside BEGIN TRANSACTION READ ONLY … ROLLBACK. Escalation is blocked by the engine.",
  },
  {
    n: "04",
    title: "Append-only audit",
    body: "Allowed, blocked, or failed — every call lands in JSONL. Independent of RUST_LOG.",
  },
];

export function SecurityStack() {
  return (
    <section className="section" id="security">
      <div className="container">
        <div className="section-head center">
          <p className="eyebrow">security model</p>
          <h2 className="section-title">Layers, not vibes</h2>
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
