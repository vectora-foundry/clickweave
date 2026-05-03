import { ChevronDown, ChevronUp, Copy, Trash2 } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { useShallow } from "zustand/react/shallow";
import { useStore } from "../../store/useAppStore";

const HEADER_HEIGHT = 36;
const EXPANDED_HEIGHT = 240;

// Mirror LogsDrawer.tsx's substring color rules for visual consistency.
function logColor(log: string): string {
  if (log.includes("Error") || log.includes("failed")) return "text-red-400";
  if (log.includes("completed") || log.includes("Saved")) return "text-[var(--accent-green)]";
  return "text-[var(--text-secondary)]";
}

export function LogsBar() {
  const { logs, open } = useStore(
    useShallow((s) => ({ logs: s.logs, open: s.logsDrawerOpen })),
  );
  const toggle = useStore((s) => s.toggleLogsDrawer);
  const clearLogs = useStore((s) => s.clearLogs);
  const [search, setSearch] = useState("");
  const scrollRef = useRef<HTMLDivElement>(null);

  const filtered = useMemo(() => {
    if (!search) return logs;
    const q = search.toLowerCase();
    return logs.filter((l) => l.toLowerCase().includes(q));
  }, [logs, search]);

  // Auto-scroll to the bottom on new log lines.
  useEffect(() => {
    if (scrollRef.current) {
      scrollRef.current.scrollTop = scrollRef.current.scrollHeight;
    }
  }, [filtered]);

  const onCopy = () => {
    void navigator.clipboard.writeText(filtered.join("\n"));
  };

  return (
    <div
      className="flex flex-col border-t border-[var(--hairline-strong)] bg-[var(--oxide)]"
      style={{ height: open ? EXPANDED_HEIGHT : HEADER_HEIGHT }}
    >
      <div className="flex h-9 items-center gap-2 border-b border-[var(--hairline)] px-3">
        <button
          onClick={toggle}
          aria-expanded={open}
          aria-label={open ? "Collapse logs" : "Expand logs"}
          className="flex items-center gap-1.5 text-[11px] font-medium tracking-[0.06em] text-[var(--text-secondary)] hover:text-[var(--text-primary)]"
        >
          {open ? (
            <ChevronDown size={12} strokeWidth={1.5} />
          ) : (
            <ChevronUp size={12} strokeWidth={1.5} />
          )}
          Logs
        </button>
        <span className="font-mono text-[10px] text-[var(--text-muted)]">{logs.length}</span>
        {open && (
          <>
            <input
              value={search}
              onChange={(e) => setSearch(e.target.value)}
              placeholder="Search logs"
              className="ml-2 h-6 flex-1 rounded border border-[var(--hairline)] bg-[var(--ink)] px-2 text-[11px] text-[var(--text-primary)] outline-none focus:border-[var(--accent-coral)]"
            />
            <button onClick={onCopy} aria-label="Copy logs" className="rounded p-1 text-[var(--text-muted)] hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]">
              <Copy size={12} strokeWidth={1.5} />
            </button>
            <button onClick={clearLogs} aria-label="Clear logs" className="rounded p-1 text-[var(--text-muted)] hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]">
              <Trash2 size={12} strokeWidth={1.5} />
            </button>
          </>
        )}
      </div>
      {open && (
        <div ref={scrollRef} className="flex-1 overflow-y-auto px-3 py-2 font-mono text-[11px]">
          {filtered.length === 0 ? (
            <div className="text-[var(--text-muted)]">{search ? "No matches" : "No logs"}</div>
          ) : (
            filtered.map((l, i) => (
              <div
                key={i}
                className={`cw-log-slide break-words whitespace-pre-wrap ${logColor(l)}`}
              >
                {l}
              </div>
            ))
          )}
        </div>
      )}
    </div>
  );
}
