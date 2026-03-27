import type { Operator, SwitchCase } from "../../../../bindings";
import { FieldGroup, TextField } from "../../fields";
import { ConditionEditor } from "./ConditionEditor";
import type { NodeEditorProps } from "./types";
import { useNodeTypeUpdater } from "./useNodeTypeUpdater";

export function SwitchEditor({ nodeType, onUpdate }: NodeEditorProps) {
  const nt = nodeType;
  if (nt.type !== "Switch") return null;

  const updateType = useNodeTypeUpdater(nt, onUpdate);

  return (
    <FieldGroup title="Switch Cases">
      {nt.cases.map((c: SwitchCase, i: number) => (
        <div
          key={i}
          className="mb-3 border-b border-[var(--border)] pb-3"
        >
          <div className="mb-1 flex items-center justify-between">
            <TextField
              label={`Case ${i + 1} Name`}
              value={c.name}
              onChange={(name) => {
                const cases = [...nt.cases];
                cases[i] = { ...cases[i], name };
                updateType({ cases });
              }}
            />
            <button
              onClick={() => {
                const cases = nt.cases.filter(
                  (_: SwitchCase, j: number) => j !== i,
                );
                updateType({ cases });
              }}
              className="ml-2 text-xs text-red-400 hover:text-red-300"
            >
              Remove
            </button>
          </div>
          <ConditionEditor
            condition={c.condition}
            onChange={(condition) => {
              const cases = [...nt.cases];
              cases[i] = { ...cases[i], condition };
              updateType({ cases });
            }}
          />
        </div>
      ))}
      <button
        onClick={() => {
          const newCase: SwitchCase = {
            name: `Case ${nt.cases.length + 1}`,
            condition: {
              left: { node: "", field: "" },
              operator: "Equals" as Operator,
              right: {
                type: "Literal" as const,
                value: { type: "Bool" as const, value: true },
              },
            } as unknown as SwitchCase["condition"],
          };
          updateType({ cases: [...nt.cases, newCase] });
        }}
        className="text-xs text-[var(--accent-coral)] hover:underline"
      >
        + Add Case
      </button>
    </FieldGroup>
  );
}
