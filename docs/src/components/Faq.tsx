const ITEMS = [
  {
    q: "Can an agent (or I) still write if we try hard enough?",
    a: "No. Writes are rejected by allowlists before they hit the wire, and Postgres adds session + transaction READ ONLY. Mongo blocks $out/$merge. Redis only allows listed read commands. There is no --write flag.",
  },
  {
    q: "Where do credentials live?",
    a: "In databases.toml via ${ENV_VAR} placeholders. Literal passwords are rejected at load time unless you explicitly set allow_literal_password for local dev. The audit log never stores connection strings.",
  },
  {
    q: "Why not just use psql / mongosh / redis-cli?",
    a: "Those tools can write. They are not multi-backend, not MCP-native, and they do not leave a structured audit trail of every agent call. peekdb is built for the AI coding loop.",
  },
  {
    q: "Which AI clients work?",
    a: "Any MCP client over stdio: Claude Code / Desktop, Cursor, and similar. The same binary is a plain CLI when you want to peek without an agent.",
  },
  {
    q: "Is this multi-tenant SaaS?",
    a: "No. peekdb runs on your machine with your credentials. Timeouts and row limits are safety rails for a single trusted user — not a shared service boundary.",
  },
];

export function Faq() {
  return (
    <section className="section" id="faq">
      <div className="container">
        <div className="section-head center">
          <p className="eyebrow">faq</p>
          <h2 className="section-title">Straight answers</h2>
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
