import { useEffect, useState } from "react";

type Frame =
  | { kind: "type"; text: string; delay?: number }
  | { kind: "lines"; lines: string[]; className?: string; delay?: number }
  | { kind: "pause"; delay: number }
  | { kind: "clear" };

const FRAMES: Frame[] = [
  {
    kind: "type",
    text: 'peekdb query --db staging --sql "SELECT id, email FROM users LIMIT 2"',
    delay: 28,
  },
  { kind: "pause", delay: 280 },
  {
    kind: "lines",
    className: "t-table",
    delay: 40,
    lines: [
      "┌────┬───────────────────┐",
      "│ id │ email             │",
      "├────┼───────────────────┤",
      "│ 1  │ alice@example.com │",
      "│ 2  │ bob@example.com   │",
      "└────┴───────────────────┘",
      "(2 rows)",
    ],
  },
  { kind: "pause", delay: 900 },
  {
    kind: "type",
    text: 'peekdb query --db staging --sql "INSERT INTO users VALUES (3,\'x\')"',
    delay: 24,
  },
  { kind: "pause", delay: 220 },
  {
    kind: "lines",
    className: "t-err",
    delay: 30,
    lines: [
      "Error: Not allowed: statement must start with",
      "SELECT, WITH, EXPLAIN, SHOW, VALUES, or TABLE",
      "(found 'INSERT')",
      "",
      "audit: outcome=blocked  backend=postgres",
    ],
  },
  { kind: "pause", delay: 1600 },
  { kind: "clear" },
];

function wait(ms: number) {
  return new Promise<void>((resolve) => {
    window.setTimeout(resolve, ms);
  });
}

export function TerminalDemo() {
  const [promptLine, setPromptLine] = useState("");
  const [output, setOutput] = useState<{ text: string; className?: string }[]>([]);
  const [typing, setTyping] = useState(true);

  useEffect(() => {
    let cancelled = false;

    async function run() {
      while (!cancelled) {
        for (const frame of FRAMES) {
          if (cancelled) return;

          if (frame.kind === "clear") {
            setPromptLine("");
            setOutput([]);
            setTyping(true);
            await wait(400);
            continue;
          }

          if (frame.kind === "pause") {
            setTyping(false);
            await wait(frame.delay);
            continue;
          }

          if (frame.kind === "type") {
            setTyping(true);
            setPromptLine("");
            for (let c = 1; c <= frame.text.length; c++) {
              if (cancelled) return;
              setPromptLine(frame.text.slice(0, c));
              await wait(frame.delay ?? 28);
            }
            setTyping(false);
            await wait(120);
            continue;
          }

          if (frame.kind === "lines") {
            for (const line of frame.lines) {
              if (cancelled) return;
              setOutput((prev) => [...prev, { text: line, className: frame.className }]);
              await wait(frame.delay ?? 35);
            }
          }
        }
      }
    }

    void run();
    return () => {
      cancelled = true;
    };
  }, []);

  return (
    <div className="terminal" aria-label="Animated peekdb terminal demo">
      <div className="terminal-bar">
        <span className="terminal-dot" />
        <span className="terminal-dot" />
        <span className="terminal-dot" />
        <span className="terminal-title">peekdb — read-only shell</span>
      </div>
      <div className="terminal-body">
        {output.map((line, idx) => (
          <div key={`${idx}-${line.text.slice(0, 16)}`} className={line.className}>
            {line.text || "\u00a0"}
          </div>
        ))}
        <div>
          <span className="t-prompt">$ </span>
          <span className="t-cmd">{promptLine}</span>
          {typing && <span className="cursor" aria-hidden />}
        </div>
      </div>
    </div>
  );
}
