import { useRef, useEffect, useState } from "react";

interface LogsDrawerProps {
  open: boolean;
  logs: string[];
  onToggle: () => void;
  onClear: () => void;
}

const MIN_HEIGHT = 100;
const MAX_HEIGHT = 600;
const DEFAULT_HEIGHT = 192;
const HEADER_HEIGHT = 32;

function logColor(log: string): string {
  if (log.includes("Error") || log.includes("failed")) return "text-red-400";
  if (log.includes("completed") || log.includes("Saved")) return "text-[var(--accent-green)]";
  return "text-[var(--text-secondary)]";
}

export function LogsDrawer({ open, logs, onToggle, onClear }: LogsDrawerProps) {
  const scrollRef = useRef<HTMLDivElement>(null);
  const [height, setHeight] = useState(DEFAULT_HEIGHT);

  useEffect(() => {
    if (scrollRef.current) {
      scrollRef.current.scrollTop = scrollRef.current.scrollHeight;
    }
  }, [logs]);

  function onResizeStart(e: React.MouseEvent) {
    e.preventDefault();
    const startY = e.clientY;
    const startH = height;

    function onMouseMove(ev: MouseEvent) {
      setHeight(Math.min(MAX_HEIGHT, Math.max(MIN_HEIGHT, startH + startY - ev.clientY)));
    }

    function onMouseUp() {
      document.body.style.cursor = "";
      document.body.style.userSelect = "";
      window.removeEventListener("mousemove", onMouseMove);
      window.removeEventListener("mouseup", onMouseUp);
    }

    document.body.style.cursor = "ns-resize";
    document.body.style.userSelect = "none";
    window.addEventListener("mousemove", onMouseMove);
    window.addEventListener("mouseup", onMouseUp);
  }

  return (
    <div
      className="flex flex-col border-t border-[var(--border)] bg-[var(--bg-panel)]"
      style={{ height: open ? height : HEADER_HEIGHT }}
    >
      {open && (
        <div
          onMouseDown={onResizeStart}
          className="group flex shrink-0 cursor-ns-resize items-center justify-center py-1"
        >
          <div className="h-px w-12 rounded-full bg-[var(--text-muted)] opacity-0 transition-opacity group-hover:opacity-60" />
        </div>
      )}

      <div className="flex h-8 shrink-0 items-center justify-between border-b border-[var(--border)] px-3">
        <button
          onClick={onToggle}
          className="flex items-center gap-2 text-xs text-[var(--text-secondary)] hover:text-[var(--text-primary)]"
        >
          <span className={`transition-transform ${open ? "rotate-180" : ""}`}>
            ^
          </span>
          <span>Logs ({logs.length})</span>
        </button>
        {open && (
          <button
            onClick={onClear}
            className="text-xs text-[var(--text-muted)] hover:text-[var(--text-primary)]"
          >
            Clear
          </button>
        )}
      </div>

      {open && (
        <div
          ref={scrollRef}
          className="min-h-0 flex-1 overflow-y-auto p-2 font-mono text-[11px] leading-relaxed select-text"
        >
          {logs.map((log, i) => (
            <div
              key={i}
              className={`py-0.5 ${logColor(log)}`}
            >
              {log}
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
