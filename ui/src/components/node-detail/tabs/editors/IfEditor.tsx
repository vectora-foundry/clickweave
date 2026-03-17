import { FieldGroup } from "../../fields";
import { ConditionEditor } from "./ConditionEditor";
import type { NodeEditorProps } from "./types";
import { useNodeTypeUpdater } from "./useNodeTypeUpdater";

export function IfEditor({ nodeType, onUpdate }: NodeEditorProps) {
  const nt = nodeType;
  if (nt.type !== "If") return null;

  const updateType = useNodeTypeUpdater(nt, onUpdate);

  return (
    <FieldGroup title="If Condition">
      <ConditionEditor
        condition={nt.condition}
        onChange={(condition) => updateType({ condition })}
      />
    </FieldGroup>
  );
}
