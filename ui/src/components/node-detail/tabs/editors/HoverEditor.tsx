import { FieldGroup, NumberField, TextField } from "../../fields";
import { APP_KIND_LABELS, type NodeEditorProps, usesCdp } from "./types";
import { useNodeTypeUpdater } from "./useNodeTypeUpdater";

export function HoverEditor({ nodeType, onUpdate, appKind }: NodeEditorProps) {
  const nt = nodeType;
  if (nt.type !== "Hover") return null;

  const updateType = useNodeTypeUpdater(nt, onUpdate);

  const isCdp = appKind && usesCdp(appKind);

  return (
    <FieldGroup title="Hover">
      {nt.target?.type === "CdpElement" ? (
        <div>
          <label className="mb-1 block text-xs text-[var(--text-secondary)]">
            Target (CDP)
          </label>
          <span className="block px-2.5 py-1.5 text-sm">
            &quot;{nt.target.name}&quot;
            {nt.target.role && <span className="ml-1 text-xs text-[var(--text-muted)]">({nt.target.role})</span>}
          </span>
          {nt.target.parent_role && (
            <span className="block px-2.5 text-[10px] text-[var(--text-muted)]">
              in {nt.target.parent_role}
              {nt.target.parent_name && <> &quot;{nt.target.parent_name}&quot;</>}
            </span>
          )}
        </div>
      ) : (
        <TextField
          label="Target"
          value={nt.target?.type === "Text" ? nt.target.text : ""}
          onChange={(v) => updateType({ target: v ? { type: "Text" as const, text: v } : null })}
          placeholder="Text to find and hover (auto-resolves coordinates)"
        />
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
      <NumberField
        label="Dwell (ms)"
        value={nt.dwell_ms}
        min={0}
        max={10000}
        onChange={(v) => updateType({ dwell_ms: v ?? 500 })}
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
