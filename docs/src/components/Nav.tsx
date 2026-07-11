const GH = "https://github.com/Harkirat155/peekdb";

export function Nav() {
  return (
    <header className="nav">
      <div className="container nav-inner">
        <a className="logo" href="#top">
          peek<span>db</span>
        </a>
        <nav className="nav-links" aria-label="Primary">
          <a href="#features">Features</a>
          <a href="#for-agents">Agents</a>
          <a href="#security">Security</a>
          <a href="#install">Install</a>
          <a href="#faq">FAQ</a>
          <a href="/peekdb/llms.txt">llms.txt</a>
        </nav>
        <div className="nav-actions">
          <a className="btn btn-ghost" href={GH} target="_blank" rel="noreferrer">
            GitHub
          </a>
          <a className="btn btn-primary" href="#install">
            Install
          </a>
        </div>
      </div>
    </header>
  );
}
