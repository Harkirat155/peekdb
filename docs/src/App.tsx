import { Faq } from "./components/Faq";
import { Footer } from "./components/Footer";
import { Hero } from "./components/Hero";
import { Install } from "./components/Install";
import { Nav } from "./components/Nav";
import { SecurityStack } from "./components/SecurityStack";

const FEATURES = [
  {
    label: "agents",
    title: "Prompt injection hits a wall",
    body: "Multi-layer allowlists and engine-level READ ONLY mean a clever prompt cannot escalate to INSERT, DROP, or FLUSHALL.",
  },
  {
    label: "credentials",
    title: "Secrets stay out of files",
    body: "${ENV_VAR} interpolation is required. Literal passwords in TOML are rejected unless you opt in for local-only hacks.",
  },
  {
    label: "vs psql",
    title: "Built for the IDE loop",
    body: "One binary, three backends, MCP + CLI, row caps, timeouts, and an audit trail. Not a general DBA shell — a safe peek.",
  },
];

const STEPS = [
  {
    title: "Configure targets",
    body: "Name your Postgres, Mongo, and Redis instances in databases.toml. Use env vars for passwords.",
  },
  {
    title: "Serve or query",
    body: "peekdb serve for MCP clients, or peekdb query / mongo / redis for one-shot peeks from the shell.",
  },
  {
    title: "Let the agent explore",
    body: "The model gets sql_query, mongo_query, and redis_query — and nothing that mutates state.",
  },
];

export default function App() {
  return (
    <div className="site">
      <div className="aurora" aria-hidden />
      <Nav />
      <main>
        <Hero />

        <section className="trust">
          <div className="container trust-inner">
            {["Postgres", "MongoDB", "Redis", "MCP + CLI", "Audit log", "MIT"].map((c) => (
              <span className="chip" key={c}>
                {c}
              </span>
            ))}
          </div>
        </section>

        <section className="section" id="features">
          <div className="container">
            <div className="section-head">
              <p className="eyebrow">why peekdb</p>
              <h2 className="section-title">Useful to you first</h2>
              <p className="section-lead">
                Built for the moment an agent needs real schema and data — without trusting a
                prompt to stay polite.
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

        <section className="section">
          <div className="container">
            <div className="section-head center">
              <p className="eyebrow">workflow</p>
              <h2 className="section-title">Three steps</h2>
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
          <section className="cta-band">
            <h2>Give your agent eyes. Keep its hands off.</h2>
            <p>
              Same binary for you in the terminal and for Claude, Cursor, or any MCP client.
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
