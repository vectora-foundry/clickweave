import { FieldGroup, TextAreaField, TextField } from "../../fields";
import type { NodeEditorProps } from "./types";
import { useNodeTypeUpdater } from "./useNodeTypeUpdater";

export function AppDebugKitOpEditor({ nodeType, onUpdate }: NodeEditorProps) {
  const nt = nodeType;
  if (nt.type !== "AppDebugKitOp") return null;

  const updateType = useNodeTypeUpdater(nt, onUpdate);

  return (
    <FieldGroup title="AppDebugKit">
      <TextField
        label="Operation Name"
        value={nt.operation_name}
        onChange={(v) => updateType({ operation_name: v })}
      />
      <TextAreaField
        label="Parameters (JSON)"
        value={
          typeof nt.parameters === "string"
            ? nt.parameters
            : JSON.stringify(nt.parameters, null, 2)
        }
        onChange={(v) => {
          try {
            updateType({ parameters: JSON.parse(v) });
          } catch {
            // Keep raw text during editing
          }
        }}
      />
    </FieldGroup>
  );
}
