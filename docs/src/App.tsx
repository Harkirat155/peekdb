import { Faq } from "./components/Faq";
import { Footer } from "./components/Footer";
import { ForAgents } from "./components/ForAgents";
import { Hero } from "./components/Hero";
import { Install } from "./components/Install";
import { Nav } from "./components/Nav";
import { SecurityStack } from "./components/SecurityStack";

const FEATURES = [
  {
    label: "agents",
    title: "Discovery before guesses",
    body: "list_targets → list_objects → describe/sample → query. Structured JSON results by default. MCP resources for schema context.",
  },
  {
    label: "security",
    title: "Prompt injection hits a wall",
    body: "Multi-layer RO: statement allowlists, Postgres READ ONLY transactions, nested Mongo $out/$merge block, Redis deny-by-default.",
  },
  {
    label: "controls",
    title: "Production-shaped knobs",
    body: "Optional allow_tables, rate limits, max_response_bytes, redact_columns — plus append-only audit and peekdb audit filters.",
  },
  {
    label: "humans",
    title: "CLI, REPL, explore",
    body: "query/mongo/redis with --format table|vertical|csv|tsv|json. Interactive repl and explore for the same safe backends.",
  },
  {
    label: "credentials",
    title: "Secrets stay out of files",
    body: "${ENV_VAR} interpolation required. Literal passwords rejected unless you opt in for local-only hacks.",
  },
  {
    label: "one binary",
    title: "MCP + CLI parity",
    body: "init, doctor, mcp-snippet, fav, completions. Same enforcement whether the caller is an agent or a shell.",
  },
];

const STEPS = [
  {
    title: "Configure targets",
    body: "peekdb init → databases.toml with Postgres, Mongo, Redis. Passwords via ${ENV_VAR}. peekdb doctor to probe.",
  },
  {
    title: "Wire MCP or shell",
    body: "peekdb mcp-snippet into Claude/Cursor, or use query / repl / explore directly.",
  },
  {
    title: "Explore safely",
    body: "Agents call discovery tools first. Humans use \\dt / sample. Writes never leave the allowlist.",
  },
];

const TRUST = [
  "Postgres",
  "MongoDB",
  "Redis",
  "MCP + CLI",
  "REPL · explore",
  "Audit log",
  "Allowlists · redact",
  "MIT",
];

export default function App() {
  return (
    <div className="site">
      <div className="aurora" aria-hidden />
      <Nav />
      <main id="main">
        <Hero />

        <section className="trust" aria-label="Product highlights">
          <div className="container trust-inner">
            {TRUST.map((c) => (
              <span className="chip" key={c}>
                {c}
              </span>
            ))}
          </div>
        </section>

        <section className="section" id="features" aria-labelledby="features-title">
          <div className="container">
            <div className="section-head">
              <p className="eyebrow">why peekdb</p>
              <h2 className="section-title" id="features-title">
                Useful to you — and your agent
              </h2>
              <p className="section-lead">
                Phases 0–3 shipped: install UX, agent discovery, safety controls, interactive
                surfaces. No write path. Ever.
              </p>
            </div>
            <div className="grid-3">
              {FEATURES.map((f) => (
                <article className="card" key={f.title}>
                  <span className="label">{f.label}</span>
                  <h3>{f.title}</h3>
                  <p>{f.body}</p>
                </article>
              ))}
            </div>
          </div>
        </section>

        <ForAgents />

        <section className="section" aria-labelledby="workflow-title">
          <div className="container">
            <div className="section-head center">
              <p className="eyebrow">workflow</p>
              <h2 className="section-title" id="workflow-title">
                Three steps
              </h2>
            </div>
            <div className="steps">
              {STEPS.map((s) => (
                <div className="step" key={s.title}>
                  <h3>{s.title}</h3>
                  <p>{s.body}</p>
                </div>
              ))}
            </div>
          </div>
        </section>

        <SecurityStack />
        <Install />
        <Faq />

        <div className="container">
          <section className="cta-band" aria-labelledby="cta-title">
            <h2 id="cta-title">Give your agent eyes. Keep its hands off.</h2>
            <p>
              Same binary for the terminal and for Claude, Cursor, Grok, or any MCP client. Start
              with{" "}
              <a href="/peekdb/llms.txt" style={{ color: "var(--accent-ink)" }}>
                llms.txt
              </a>{" "}
              if you are an agent.
            </p>
            <div className="hero-ctas">
              <a className="btn btn-primary" href="#install">
                Install peekdb
              </a>
              <a
                className="btn btn-ghost"
                href="https://github.com/Harkirat155/peekdb"
                target="_blank"
                rel="noreferrer"
              >
                Star on GitHub
              </a>
            </div>
          </section>
        </div>
      </main>
      <Footer />
    </div>
  );
}
