import { useEffect, useRef, useState } from "react";
import { GROUP_PALETTE } from "../utils/color";

export interface GroupContextMenuItem {
  label: string;
  action?: () => void;
  danger?: boolean;
  disabled?: boolean;
  /** If provided, renders a color palette row instead of calling action() directly. */
  colorPicker?: {
    currentColor: string;
    onPickColor: (color: string) => void;
  };
}

interface GroupContextMenuProps {
  position: { x: number; y: number };
  items: GroupContextMenuItem[];
  onClose: () => void;
}

export function GroupContextMenu({ position, items, onClose }: GroupContextMenuProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const [expandedColorPicker, setExpandedColorPicker] = useState<string | null>(null);

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
        <div key={item.label}>
          <button
            type="button"
            disabled={item.disabled}
            onClick={() => {
              if (item.colorPicker) {
                setExpandedColorPicker((prev) => (prev === item.label ? null : item.label));
              } else if (item.action) {
                item.action();
                onClose();
              }
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
          {item.colorPicker && expandedColorPicker === item.label && (
            <div className="flex gap-1.5 px-3 py-1.5">
              {GROUP_PALETTE.map((color) => (
                <button
                  key={color}
                  type="button"
                  onClick={() => {
                    item.colorPicker!.onPickColor(color);
                    onClose();
                  }}
                  className="h-5 w-5 rounded-full border-2 transition-transform hover:scale-110"
                  style={{
                    backgroundColor: color,
                    borderColor: color === item.colorPicker!.currentColor ? "white" : "transparent",
                  }}
                />
              ))}
            </div>
          )}
        </div>
      ))}
    </div>
  );
}
