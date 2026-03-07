import { useCallback, useMemo } from "react";
import { commands } from "../../../bindings";

export function FieldGroup({
  title,
  children,
}: {
  title: string;
  children: React.ReactNode;
}) {
  return (
    <div>
      <h3 className="mb-2 text-xs font-semibold uppercase tracking-wider text-[var(--text-muted)]">
        {title}
      </h3>
      <div className="space-y-2">{children}</div>
    </div>
  );
}

export function TextField({
  label,
  value,
  onChange,
  placeholder,
}: {
  label: string;
  value: string;
  onChange: (v: string) => void;
  placeholder?: string;
}) {
  return (
    <div>
      <label className="mb-1 block text-xs text-[var(--text-secondary)]">
        {label}
      </label>
      <input
        type="text"
        value={value}
        onChange={(e) => onChange(e.target.value)}
        placeholder={placeholder}
        className="w-full rounded bg-[var(--bg-input)] px-2.5 py-1.5 text-xs text-[var(--text-primary)] placeholder-[var(--text-muted)] outline-none focus:ring-1 focus:ring-[var(--accent-coral)]"
      />
    </div>
  );
}

export function TextAreaField({
  label,
  value,
  onChange,
}: {
  label: string;
  value: string;
  onChange: (v: string) => void;
}) {
  return (
    <div>
      <label className="mb-1 block text-xs text-[var(--text-secondary)]">
        {label}
      </label>
      <textarea
        value={value}
        onChange={(e) => onChange(e.target.value)}
        rows={4}
        className="w-full rounded bg-[var(--bg-input)] px-2.5 py-1.5 text-xs text-[var(--text-primary)] outline-none focus:ring-1 focus:ring-[var(--accent-coral)] font-mono resize-y"
      />
    </div>
  );
}

export function NumberField({
  label,
  value,
  onChange,
  min,
  max,
  step,
}: {
  label: string;
  value: number;
  onChange: (v: number) => void;
  min?: number;
  max?: number;
  step?: number;
}) {
  return (
    <div>
      <label className="mb-1 block text-xs text-[var(--text-secondary)]">
        {label}
      </label>
      <input
        type="number"
        value={value}
        onChange={(e) => onChange(Number(e.target.value))}
        min={min}
        max={max}
        step={step}
        className="w-full rounded bg-[var(--bg-input)] px-2.5 py-1.5 text-xs text-[var(--text-primary)] outline-none focus:ring-1 focus:ring-[var(--accent-coral)]"
      />
    </div>
  );
}

export function CheckboxField({
  label,
  value,
  onChange,
}: {
  label: string;
  value: boolean;
  onChange: (v: boolean) => void;
}) {
  return (
    <label className="flex items-center gap-2 cursor-pointer">
      <input
        type="checkbox"
        checked={value}
        onChange={(e) => onChange(e.target.checked)}
        className="rounded border-[var(--border)] bg-[var(--bg-input)] accent-[var(--accent-coral)]"
      />
      <span className="text-xs text-[var(--text-secondary)]">{label}</span>
    </label>
  );
}

export function SelectField({
  label,
  value,
  options,
  labels,
  onChange,
}: {
  label: string;
  value: string;
  options: string[];
  labels?: Record<string, string>;
  onChange: (v: string) => void;
}) {
  return (
    <div>
      <label className="mb-1 block text-xs text-[var(--text-secondary)]">
        {label}
      </label>
      <select
        value={value}
        onChange={(e) => onChange(e.target.value)}
        className="w-full rounded bg-[var(--bg-input)] px-2.5 py-1.5 text-xs text-[var(--text-primary)] outline-none focus:ring-1 focus:ring-[var(--accent-coral)]"
      >
        {options.map((opt) => (
          <option key={opt} value={opt}>
            {labels?.[opt] ?? opt}
          </option>
        ))}
      </select>
    </div>
  );
}

export function ImagePathField({
  label,
  value,
  projectPath,
  onChange,
}: {
  label: string;
  value: string;
  projectPath: string | null;
  onChange: (v: string) => void;
}) {
  const handleBrowse = useCallback(async () => {
    if (!projectPath) return;
    const result = await commands.importAsset(projectPath);
    if (result.status === "ok" && result.data) {
      onChange(result.data.relative_path);
    }
  }, [projectPath, onChange]);

  return (
    <div>
      <label className="mb-1 block text-xs text-[var(--text-secondary)]">
        {label}
      </label>
      <div className="flex gap-1.5">
        <input
          type="text"
          value={value}
          onChange={(e) => onChange(e.target.value)}
          placeholder={projectPath ? "Select an image..." : "Save project first"}
          className="flex-1 rounded bg-[var(--bg-input)] px-2.5 py-1.5 text-xs text-[var(--text-primary)] placeholder-[var(--text-muted)] outline-none focus:ring-1 focus:ring-[var(--accent-coral)]"
        />
        <button
          onClick={handleBrowse}
          disabled={!projectPath}
          className="rounded bg-[var(--bg-input)] px-2.5 py-1.5 text-xs text-[var(--text-secondary)] hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)] disabled:opacity-40 disabled:cursor-not-allowed"
        >
          Browse
        </button>
        {value && (
          <button
            onClick={() => onChange("")}
            className="rounded bg-[var(--bg-input)] px-2 py-1.5 text-xs text-red-400 hover:bg-red-500/20"
          >
            Clear
          </button>
        )}
      </div>
      <ImagePreview value={value} />
    </div>
  );
}

/** Renders an inline preview when the value is base64-encoded image data. */
function ImagePreview({ value }: { value: string }) {
  const src = useMemo(() => {
    if (!value || value.length < 64) return null;
    // Skip if it looks like a file path (e.g. "assets/icon.png")
    if (/\.\w{1,5}$/.test(value) || value.startsWith("./") || value.startsWith("../")) return null;
    // Detect JPEG (/9j/) or PNG (iVBOR) base64 headers
    const mime = value.startsWith("/9j/") ? "image/jpeg"
      : value.startsWith("iVBOR") ? "image/png"
      : null;
    if (!mime) return null;
    return `data:${mime};base64,${value}`;
  }, [value]);

  if (!src) return null;

  return (
    <img
      src={src}
      alt="Template preview"
      className="mt-1.5 max-h-32 rounded border border-[var(--border)] object-contain"
    />
  );
}

export function EmptyState({ message }: { message: string }) {
  return (
    <div className="flex h-32 items-center justify-center text-xs text-[var(--text-muted)]">
      {message}
    </div>
  );
}

export function StatusBadge({ status }: { status: string }) {
  const colors =
    {
      Ok: "bg-[var(--accent-green)]/20 text-[var(--accent-green)]",
      Failed: "bg-red-500/20 text-red-400",
    }[status] ?? "bg-yellow-500/20 text-yellow-400";
  return (
    <span className={`rounded px-2 py-0.5 text-[10px] font-medium ${colors}`}>
      {status}
    </span>
  );
}
