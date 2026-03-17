import { FieldGroup, TextField } from "../../fields";
import type { NodeEditorProps } from "./types";
import { useNodeTypeUpdater } from "./useNodeTypeUpdater";

export function PressKeyEditor({ nodeType, onUpdate }: NodeEditorProps) {
  const nt = nodeType;
  if (nt.type !== "PressKey") return null;

  const updateType = useNodeTypeUpdater(nt, onUpdate);

  return (
    <FieldGroup title="Press Key">
      <TextField
        label="Key"
        value={nt.key}
        onChange={(v) => updateType({ key: v })}
        placeholder="e.g. return, tab, escape, a"
      />
      <TextField
        label="Modifiers"
        value={nt.modifiers.join(", ")}
        onChange={(v) =>
          updateType({
            modifiers: v ? v.split(",").map((s: string) => s.trim()).filter(Boolean) : [],
          })
        }
        placeholder="e.g. command, shift, control, option"
      />
    </FieldGroup>
  );
}
