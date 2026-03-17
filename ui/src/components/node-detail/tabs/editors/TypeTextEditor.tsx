import { FieldGroup, TextAreaField } from "../../fields";
import type { NodeEditorProps } from "./types";
import { useNodeTypeUpdater } from "./useNodeTypeUpdater";

export function TypeTextEditor({ nodeType, onUpdate }: NodeEditorProps) {
  const nt = nodeType;
  if (nt.type !== "TypeText") return null;

  const updateType = useNodeTypeUpdater(nt, onUpdate);

  return (
    <FieldGroup title="Type Text">
      <TextAreaField
        label="Text"
        value={nt.text}
        onChange={(v) => updateType({ text: v })}
      />
    </FieldGroup>
  );
}
