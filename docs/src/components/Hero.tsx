import { TerminalDemo } from "./TerminalDemo";

const GH = "https://github.com/Harkirat155/peekdb";

export function Hero() {
  return (
    <section className="hero container" id="top" aria-labelledby="hero-title">
      <p className="eyebrow">read-only · by construction · MCP + CLI</p>
      <h1 id="hero-title">Peek any database. Never write.</h1>
      <p className="hero-sub">
        Enforced read-only access to Postgres, MongoDB, and Redis for coding agents and humans —
        multi-layer allowlists, discovery tools, audit log, optional PII redaction and rate
        limits.
      </p>
      <div className="hero-ctas">
        <a className="btn btn-primary" href="#install">
          Install peekdb
        </a>
        <a className="btn btn-ghost" href="#for-agents">
          For agents
        </a>
        <a className="btn btn-ghost" href={GH} target="_blank" rel="noreferrer">
          GitHub
        </a>
      </div>
      <div className="hero-terminal">
        <TerminalDemo />
      </div>
    </section>
  );
}
