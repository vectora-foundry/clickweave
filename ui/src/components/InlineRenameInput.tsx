import { useCallback, useEffect, useRef, useState } from "react";

interface InlineRenameInputProps {
  label: string;
  onConfirm: (newName: string) => void;
  onCancel: () => void;
}

export function InlineRenameInput({ label, onConfirm, onCancel }: InlineRenameInputProps) {
  const [value, setValue] = useState(label);
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    requestAnimationFrame(() => inputRef.current?.focus());
  }, []);

  const confirm = useCallback(() => {
    if (value.trim()) onConfirm(value.trim());
    else onCancel();
  }, [value, onConfirm, onCancel]);

  return (
    <input
      ref={inputRef}
      type="text"
      value={value}
      onChange={(e) => setValue(e.target.value)}
      onKeyDown={(e) => {
        e.stopPropagation();
        if (e.key === "Enter") confirm();
        else if (e.key === "Escape") onCancel();
      }}
      onBlur={confirm}
      className="w-24 rounded border border-[var(--border)] bg-[var(--bg-dark)] px-1 py-0 text-xs font-medium text-[var(--text-primary)] outline-none focus:border-[var(--accent)]"
    />
  );
}
