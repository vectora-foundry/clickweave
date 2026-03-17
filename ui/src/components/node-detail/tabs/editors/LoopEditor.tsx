import { FieldGroup, NumberField } from "../../fields";
import { ConditionEditor } from "./ConditionEditor";
import type { NodeEditorProps } from "./types";
import { useNodeTypeUpdater } from "./useNodeTypeUpdater";

export function LoopEditor({ nodeType, onUpdate }: NodeEditorProps) {
  const nt = nodeType;
  if (nt.type !== "Loop") return null;

  const updateType = useNodeTypeUpdater(nt, onUpdate);

  return (
    <FieldGroup title="Loop">
      <p className="mb-2 text-[10px] text-[var(--text-muted)]">
        Loop body runs at least once (do-while). Exit condition is checked
        on every subsequent pass.
      </p>
      <ConditionEditor
        condition={nt.exit_condition}
        onChange={(exit_condition) => updateType({ exit_condition })}
      />
      <NumberField
        label="Max Iterations"
        value={nt.max_iterations}
        min={1}
        max={10000}
        onChange={(max_iterations) => updateType({ max_iterations })}
      />
    </FieldGroup>
  );
}
