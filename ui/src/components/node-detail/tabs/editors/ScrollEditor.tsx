import { FieldGroup, NumberField } from "../../fields";
import type { NodeEditorProps } from "./types";
import { useNodeTypeUpdater } from "./useNodeTypeUpdater";

export function ScrollEditor({ nodeType, onUpdate }: NodeEditorProps) {
  const nt = nodeType;
  if (nt.type !== "Scroll") return null;

  const updateType = useNodeTypeUpdater(nt, onUpdate);

  return (
    <FieldGroup title="Scroll">
      <NumberField
        label="Delta Y"
        value={nt.delta_y}
        min={-1000}
        max={1000}
        onChange={(v) => updateType({ delta_y: v })}
      />
      <NumberField
        label="X Position"
        value={nt.x ?? 0}
        onChange={(v) => updateType({ x: v === 0 ? null : v })}
      />
      <NumberField
        label="Y Position"
        value={nt.y ?? 0}
        onChange={(v) => updateType({ y: v === 0 ? null : v })}
      />
    </FieldGroup>
  );
}
