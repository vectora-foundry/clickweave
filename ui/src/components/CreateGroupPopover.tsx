import { useCallback, useEffect, useRef, useState } from "react";
import { GROUP_PALETTE } from "../utils/color";

interface CreateGroupPopoverProps {
  position: { x: number; y: number };
  defaultColorIndex: number;
  onConfirm: (name: string, color: string) => void;
  onCancel: () => void;
}

export function CreateGroupPopover({
  position,
  defaultColorIndex,
  onConfirm,
  onCancel,
}: CreateGroupPopoverProps) {
  const [name, setName] = useState("");
  const [colorIndex, setColorIndex] = useState(defaultColorIndex % GROUP_PALETTE.length);
  const inputRef = useRef<HTMLInputElement>(null);
  const containerRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  // Close when clicking outside
  useEffect(() => {
    function handleMouseDown(e: MouseEvent) {
      if (containerRef.current && !containerRef.current.contains(e.target as Node)) {
        onCancel();
      }
    }
    document.addEventListener("mousedown", handleMouseDown);
    return () => document.removeEventListener("mousedown", handleMouseDown);
  }, [onCancel]);

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        onCancel();
      } else if (e.key === "Tab") {
        e.preventDefault();
        setColorIndex((prev) => (prev + 1) % GROUP_PALETTE.length);
      } else if (e.key === "Enter") {
        e.preventDefault();
        if (name.trim()) {
          onConfirm(name.trim(), GROUP_PALETTE[colorIndex]);
        }
      }
    },
    [name, colorIndex, onConfirm, onCancel],
  );

  return (
    <div
      ref={containerRef}
      className="absolute z-50 rounded-lg border border-[var(--border)] bg-[var(--bg-panel)] p-3 shadow-xl"
      style={{ left: position.x, top: position.y, minWidth: 240 }}
      onKeyDown={handleKeyDown}
    >
      <label className="mb-1 block text-xs font-medium text-[var(--text-muted)]">
        Group name
      </label>
      <input
        ref={inputRef}
        type="text"
        value={name}
        onChange={(e) => setName(e.target.value)}
        placeholder="My Group"
        className="mb-3 w-full rounded border border-[var(--border)] bg-[var(--bg-dark)] px-2 py-1 text-sm text-[var(--text-primary)] outline-none focus:border-[var(--accent)]"
      />

      <label className="mb-1 block text-xs font-medium text-[var(--text-muted)]">
        Color <span className="text-[10px] opacity-60">(Tab to cycle)</span>
      </label>
      <div className="mb-3 flex gap-2">
        {GROUP_PALETTE.map((color, i) => (
          <button
            key={color}
            type="button"
            tabIndex={-1}
            onClick={() => setColorIndex(i)}
            className="h-6 w-6 rounded-full border-2 transition-transform"
            style={{
              backgroundColor: color,
              borderColor: i === colorIndex ? "white" : "transparent",
              transform: i === colorIndex ? "scale(1.2)" : "scale(1)",
            }}
          />
        ))}
      </div>

      <div className="flex justify-end gap-2">
        <button
          type="button"
          onClick={onCancel}
          className="rounded px-3 py-1 text-xs text-[var(--text-muted)] hover:bg-[rgba(255,255,255,0.05)] hover:text-[var(--text-primary)]"
        >
          Cancel
        </button>
        <button
          type="button"
          onClick={() => {
            if (name.trim()) onConfirm(name.trim(), GROUP_PALETTE[colorIndex]);
          }}
          disabled={!name.trim()}
          className="rounded px-3 py-1 text-xs font-medium text-white disabled:opacity-40"
          style={{ backgroundColor: GROUP_PALETTE[colorIndex] }}
        >
          Create
        </button>
      </div>
    </div>
  );
}
