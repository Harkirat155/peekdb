/** Static terminal snapshot — no per-keystroke React state (keeps TBT low). */
export function TerminalDemo() {
  return (
    <div className="terminal" aria-label="peekdb terminal demo" role="img">
      <div className="terminal-bar">
        <span className="terminal-dot" />
        <span className="terminal-dot" />
        <span className="terminal-dot" />
        <span className="terminal-title">peekdb — read-only shell</span>
      </div>
      <div className="terminal-body">
        <div>
          <span className="t-prompt">$ </span>
          <span className="t-cmd">peekdb doctor</span>
        </div>
        <div className="t-ok">ok    postgres  staging  —  SELECT 1 ok</div>
        <div className="t-ok">ok    mongo     app      —  list_collections ok</div>
        <div className="t-ok">ok    redis     cache    —  PING ok</div>
        <div className="t-ok">ok  3/3 targets reachable</div>
        <div className="t-muted"> </div>
        <div>
          <span className="t-prompt">$ </span>
          <span className="t-cmd">
            peekdb query --sql &quot;SELECT id, email FROM users LIMIT 2&quot; --format json
          </span>
        </div>
        <div className="t-table">[ {"{"} &quot;id&quot;: &quot;1&quot;, &quot;email&quot;: &quot;[REDACTED]&quot; {"}"} ]</div>
        <div className="t-muted">(2 rows, redacted)</div>
        <div className="t-muted"> </div>
        <div>
          <span className="t-prompt">$ </span>
          <span className="t-cmd">peekdb query --sql &quot;INSERT INTO users …&quot;</span>
        </div>
        <div className="t-err">Error: Not allowed (found &apos;INSERT&apos;)</div>
        <div className="t-err">audit: outcome=blocked  backend=postgres</div>
      </div>
    </div>
  );
}
