import { useState, useRef, useEffect } from "react";
import { useStore } from "../../store/useAppStore";

export function WorkflowRow() {
  const workflow = useStore((s) => s.workflow);
  const setWorkflow = useStore((s) => s.setWorkflow);
  const pushHistory = useStore((s) => s.pushHistory);

  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(workflow.name);
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (editing) inputRef.current?.focus();
  }, [editing]);

  const startEdit = () => {
    setDraft(workflow.name);
    setEditing(true);
  };

  const commit = () => {
    const next = draft.trim();
    setEditing(false);
    if (!next || next === workflow.name) return;
    pushHistory("Rename Workflow");
    setWorkflow({ ...workflow, name: next });
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
          title={workflow.name}
        >
          {workflow.name}
        </h1>
      )}
      <button
        onClick={startEdit}
        title="Rename workflow"
        aria-label="Rename workflow"
        className="shrink-0 rounded p-1 text-[var(--text-muted)] hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]"
      >
        <svg
          width="12"
          height="12"
          viewBox="0 0 16 16"
          fill="none"
          stroke="currentColor"
          strokeWidth="1.3"
          strokeLinecap="round"
          strokeLinejoin="round"
        >
          <path d="M11.5 2.5l2 2L5 13l-3 1 1-3 8.5-8.5z" />
        </svg>
      </button>
    </div>
  );
}
