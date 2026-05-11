import { Pencil } from "lucide-react";
import { useState, useRef, useEffect } from "react";
import { useStore } from "../../store/useAppStore";

export function WorkflowRow() {
  const projectName = useStore((s) => s.projectName);
  const setProjectName = useStore((s) => s.setProjectName);

  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(projectName);
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (editing) inputRef.current?.focus();
  }, [editing]);

  const startEdit = () => {
    setDraft(projectName);
    setEditing(true);
  };

  const commit = () => {
    const next = draft.trim();
    setEditing(false);
    if (!next || next === projectName) return;
    setProjectName(next);
  };

  return (
    <div className="flex min-w-0 items-center gap-2 px-6 py-2">
      {editing ? (
        <input
          ref={inputRef}
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          onBlur={commit}
          onKeyDown={(e) => {
            if (e.key === "Enter") commit();
            if (e.key === "Escape") setEditing(false);
          }}
          className="min-w-0 flex-1 bg-transparent text-[15px] font-medium text-[var(--text-primary)] outline-none"
        />
      ) : (
        <h1
          className="min-w-0 truncate text-[15px] font-medium text-[var(--text-primary)]"
          title={projectName}
        >
          {projectName}
        </h1>
      )}
      <button
        onClick={startEdit}
        title="Rename workflow"
        aria-label="Rename workflow"
        className="shrink-0 rounded p-1 text-[var(--text-muted)] hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]"
      >
        <Pencil size={12} strokeWidth={1.5} />
      </button>
    </div>
  );
}
