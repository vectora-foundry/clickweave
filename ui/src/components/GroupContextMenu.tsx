import { useEffect, useRef } from "react";

export interface GroupContextMenuItem {
  label: string;
  action: () => void;
  danger?: boolean;
  disabled?: boolean;
}

interface GroupContextMenuProps {
  position: { x: number; y: number };
  items: GroupContextMenuItem[];
  onClose: () => void;
}

export function GroupContextMenu({ position, items, onClose }: GroupContextMenuProps) {
  const containerRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    function handleMouseDown(e: MouseEvent) {
      if (containerRef.current && !containerRef.current.contains(e.target as Node)) {
        onClose();
      }
    }
    document.addEventListener("mousedown", handleMouseDown);
    return () => document.removeEventListener("mousedown", handleMouseDown);
  }, [onClose]);

  useEffect(() => {
    function handleKeyDown(e: KeyboardEvent) {
      if (e.key === "Escape") {
        e.preventDefault();
        onClose();
      }
    }
    document.addEventListener("keydown", handleKeyDown);
    return () => document.removeEventListener("keydown", handleKeyDown);
  }, [onClose]);

  return (
    <div
      ref={containerRef}
      className="absolute z-50 min-w-[160px] rounded-lg border border-[var(--border)] bg-[var(--bg-panel)] py-1 shadow-xl"
      style={{ left: position.x, top: position.y }}
    >
      {items.map((item) => (
        <button
          key={item.label}
          type="button"
          disabled={item.disabled}
          onClick={() => {
            item.action();
            onClose();
          }}
          className={`w-full px-3 py-1.5 text-left text-xs transition-colors ${
            item.disabled
              ? "cursor-not-allowed opacity-40"
              : item.danger
                ? "text-red-400 hover:bg-red-500/10 hover:text-red-300"
                : "text-[var(--text-primary)] hover:bg-[rgba(255,255,255,0.06)]"
          }`}
        >
          {item.label}
        </button>
      ))}
    </div>
  );
}
