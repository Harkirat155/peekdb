import { TerminalDemo } from "./TerminalDemo";

const GH = "https://github.com/Harkirat155/peekdb";

export function Hero() {
  return (
    <section className="hero container" id="top">
      <p className="eyebrow">read-only · by construction</p>
      <h1>Peek any database. Never write.</h1>
      <p className="hero-sub">
        Enforced read-only CLI and MCP for Postgres, MongoDB, and Redis — so you and your
        coding agent can explore production-shaped data without a write path to find.
      </p>
      <div className="hero-ctas">
        <a className="btn btn-primary" href="#install">
          Install peekdb
        </a>
        <a className="btn btn-ghost" href={GH} target="_blank" rel="noreferrer">
          View on GitHub
        </a>
      </div>
      <div className="hero-terminal">
        <TerminalDemo />
      </div>
    </section>
  );
}
