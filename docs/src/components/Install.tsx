import { useState } from "react";

function CopyBlock({
  title,
  code,
  highlight,
  id,
}: {
  title: string;
  code: string;
  highlight?: string[];
  id?: string;
}) {
  const [copied, setCopied] = useState(false);

  async function onCopy() {
    try {
      await navigator.clipboard.writeText(code);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1600);
    } catch {
      /* ignore */
    }
  }

  const html = highlight
    ? code.split("\n").map((line, i) => {
        const hit = highlight.some((h) => line.includes(h));
        return (
          <span key={i} className={hit ? "hl" : undefined}>
            {line}
            {"\n"}
          </span>
        );
      })
    : code;

  return (
    <div className="code-block" id={id}>
      <header>
        <span>{title}</span>
        <button
          type="button"
          className={`copy-btn${copied ? " copied" : ""}`}
          onClick={onCopy}
          aria-label={copied ? "Copied" : `Copy ${title}`}
        >
          {copied ? "copied" : "copy"}
        </button>
      </header>
      <pre>
        <code>{html}</code>
      </pre>
    </div>
  );
}

const CARGO = `# crates.io (when published) or git:
cargo install peekdb
# or: cargo install --git https://github.com/Harkirat155/peekdb
# or: cargo binstall peekdb   # if cargo-binstall is installed
# or: GitHub Releases binary for your OS
peekdb init
# edit ~/.config/peekdb/databases.toml — use \${ENV_VAR} for passwords
peekdb doctor
peekdb mcp-snippet`;

const MCP = `{
  "mcpServers": {
    "peekdb": {
      "command": "/absolute/path/to/peekdb",
      "args": ["--config", "/absolute/path/to/databases.toml"],
      "env": {
        "STAGING_DB_PASSWORD": "...",
        "RUST_LOG": "warn"
      }
    }
  }
}`;

const HUMAN = `peekdb query --db staging --sql "SELECT * FROM orders LIMIT 5" --format json
peekdb repl --db staging          # \\dt  \\d  \\sample  \\fav
peekdb explore --db staging
peekdb audit --blocked
peekdb fav save daily --sql "SELECT now()"`;

const SAFETY = `# optional per-target knobs in databases.toml
allow_tables = ["users", "orders"]
max_calls_per_minute = 60
max_rows_per_minute = 5000
max_response_bytes = 1048576
redact_columns = ["email", "ssn"]
redact_patterns = ["*password*"]`;

export function Install() {
  return (
    <section className="section" id="install" aria-labelledby="install-title">
      <div className="container">
        <div className="section-head">
          <p className="eyebrow">get started</p>
          <h2 className="section-title" id="install-title">
            Install in a minute
          </h2>
          <p className="section-lead">
            Same binary for agents (MCP) and humans (CLI / REPL / explore). Config discovery:
            <code className="inline-code"> --config</code> →{" "}
            <code className="inline-code">$PEEKDB_CONFIG</code> →{" "}
            <code className="inline-code">./databases.toml</code> →{" "}
            <code className="inline-code">~/.config/peekdb/</code>.
          </p>
        </div>
        <div className="install-grid">
          <CopyBlock
            id="install-cargo"
            title="install + doctor"
            code={CARGO}
            highlight={["cargo install", "peekdb init", "peekdb doctor", "mcp-snippet"]}
          />
          <CopyBlock
            id="install-human"
            title="human CLI"
            code={HUMAN}
            highlight={["peekdb query", "repl", "explore", "audit", "fav"]}
          />
          <div style={{ gridColumn: "1 / -1" }}>
            <CopyBlock
              id="install-mcp"
              title="MCP — Claude / Cursor / any stdio client"
              code={MCP}
              highlight={['"peekdb"', "command", "args"]}
            />
          </div>
          <div style={{ gridColumn: "1 / -1" }}>
            <CopyBlock
              id="install-safety"
              title="optional safety (Phase 2)"
              code={SAFETY}
              highlight={["allow_tables", "redact_columns", "max_calls"]}
            />
          </div>
        </div>
        <p className="install-footnote">
          Full reference:{" "}
          <a href="https://github.com/Harkirat155/peekdb#readme" target="_blank" rel="noreferrer">
            README
          </a>
          {" · "}
          <a href="/peekdb/llms.txt">llms.txt for agents</a>
          {" · "}
          <a
            href="https://github.com/Harkirat155/peekdb/blob/main/ROADMAP.md"
            target="_blank"
            rel="noreferrer"
          >
            ROADMAP
          </a>
        </p>
      </div>
    </section>
  );
}
