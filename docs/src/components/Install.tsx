import { useState } from "react";

function CopyBlock({
  title,
  code,
  highlight,
}: {
  title: string;
  code: string;
  highlight?: string[];
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
    <div className="code-block">
      <header>
        <span>{title}</span>
        <button type="button" className={`copy-btn${copied ? " copied" : ""}`} onClick={onCopy}>
          {copied ? "copied" : "copy"}
        </button>
      </header>
      <pre>{html}</pre>
    </div>
  );
}

const CARGO = `cargo install --git https://github.com/Harkirat155/peekdb
# or: download a release binary for your OS
peekdb init
peekdb doctor`;

const MCP = `{
  "mcpServers": {
    "peekdb": {
      "command": "/path/to/peekdb",
      "args": ["--config", "/path/to/databases.toml"],
      "env": {
        "STAGING_DB_PASSWORD": "...",
        "RUST_LOG": "warn"
      }
    }
  }
}`;

const QUERY = `peekdb query --db staging --sql "SELECT * FROM orders LIMIT 5"
# or: peekdb mcp-snippet  → paste into Claude / Cursor`;

export function Install() {
  return (
    <section className="section" id="install">
      <div className="container">
        <div className="section-head">
          <p className="eyebrow">get started</p>
          <h2 className="section-title">Install in a minute</h2>
          <p className="section-lead">
            Binary or cargo. Point a TOML config at your databases. Wire the same binary into
            Claude, Cursor, or any MCP client.
          </p>
        </div>
        <div className="install-grid">
          <CopyBlock title="install" code={CARGO} highlight={["cargo install"]} />
          <CopyBlock title="CLI query" code={QUERY} highlight={["peekdb query"]} />
          <div style={{ gridColumn: "1 / -1" }}>
            <CopyBlock
              title="MCP — ~/.claude/mcp.json (or Cursor equivalent)"
              code={MCP}
              highlight={['"peekdb"', "command", "args"]}
            />
          </div>
        </div>
        <p style={{ marginTop: "1.25rem", color: "var(--fg-dim)", fontSize: "0.9rem" }}>
          Full reference, security model, and config examples:{" "}
          <a
            href="https://github.com/Harkirat155/peekdb#readme"
            target="_blank"
            rel="noreferrer"
            style={{ color: "var(--accent-ink)" }}
          >
            README on GitHub
          </a>
          .
        </p>
      </div>
    </section>
  );
}
