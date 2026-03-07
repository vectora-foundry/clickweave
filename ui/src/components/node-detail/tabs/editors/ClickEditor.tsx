import { useMemo } from "react";
import type { NodeType } from "../../../../bindings";
import { FieldGroup, NumberField, SelectField, TextField } from "../../fields";
import { APP_KIND_LABELS, type NodeEditorProps, optionalString, usesCdp } from "./types";

export function ClickEditor({ nodeType, onUpdate, projectPath, appKind }: NodeEditorProps) {
  const nt = nodeType;
  if (nt.type !== "Click") return null;

  const updateType = (patch: Record<string, unknown>) => {
    onUpdate({ node_type: { ...nt, ...patch } as NodeType });
  };

  const hasImage = !!nt.template_image;
  const isCdp = appKind && usesCdp(appKind);

  return (
    <FieldGroup title="Click">
      <TextField
        label="Target"
        value={nt.target ?? ""}
        onChange={(v) => updateType({ target: optionalString(v) })}
        placeholder="Text to find and click (auto-resolves coordinates)"
      />
      <TemplateImageField
        value={nt.template_image ?? null}
        onClear={() => updateType({ template_image: null })}
      />
      {hasImage && (
        <p className="text-[10px] text-[var(--text-muted)]">
          At runtime this node uses <strong>find_image</strong> to locate the template and click at the matched coordinates.
        </p>
      )}
      <NumberField
        label="X"
        value={nt.x ?? 0}
        onChange={(v) => updateType({ x: v ?? null })}
      />
      <NumberField
        label="Y"
        value={nt.y ?? 0}
        onChange={(v) => updateType({ y: v ?? null })}
      />
      <SelectField
        label="Button"
        value={nt.button}
        options={["Left", "Right", "Center"]}
        onChange={(v) => updateType({ button: v })}
      />
      <NumberField
        label="Click Count"
        value={nt.click_count}
        min={1}
        max={3}
        onChange={(v) => updateType({ click_count: v })}
      />
      {isCdp && (
        <div>
          <label className="mb-1 block text-xs text-[var(--text-secondary)]">
            Automation
          </label>
          <span className="block px-2.5 py-1.5 text-xs text-[var(--accent-coral)]">
            {APP_KIND_LABELS[appKind]}
          </span>
        </div>
      )}
    </FieldGroup>
  );
}

function TemplateImageField({
  value,
  onClear,
}: {
  value: string | null;
  onClear: () => void;
}) {
  const src = useMemo(() => {
    if (!value || value.length < 64) return null;
    const mime = value.startsWith("/9j/")
      ? "image/jpeg"
      : value.startsWith("iVBOR")
        ? "image/png"
        : null;
    if (!mime) return null;
    return `data:${mime};base64,${value}`;
  }, [value]);

  return (
    <div>
      <label className="mb-1 block text-xs text-[var(--text-secondary)]">
        Template Image
      </label>
      {src ? (
        <div className="flex items-start gap-2">
          <img
            src={src}
            alt="Template preview"
            className="max-h-32 rounded border border-[var(--border)] object-contain"
          />
          <button
            onClick={onClear}
            className="rounded bg-[var(--bg-input)] px-2 py-1.5 text-xs text-red-400 hover:bg-red-500/20"
          >
            Clear
          </button>
        </div>
      ) : (
        <p className="text-[10px] text-[var(--text-muted)]">
          No template image. Record a walkthrough to generate one.
        </p>
      )}
    </div>
  );
}
