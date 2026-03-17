import { FieldGroup, TextField } from "../../fields";
import { type NodeEditorProps, optionalString } from "./types";
import { useNodeTypeUpdater } from "./useNodeTypeUpdater";

export function ListWindowsEditor({ nodeType, onUpdate }: NodeEditorProps) {
  const nt = nodeType;
  if (nt.type !== "ListWindows") return null;

  const updateType = useNodeTypeUpdater(nt, onUpdate);

  return (
    <FieldGroup title="List Windows">
      <TextField
        label="App Name Filter"
        value={nt.app_name ?? ""}
        onChange={(v) => updateType({ app_name: optionalString(v) })}
        placeholder="Optional"
      />
    </FieldGroup>
  );
}
