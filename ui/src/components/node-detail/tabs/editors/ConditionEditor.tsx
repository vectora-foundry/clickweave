import { useMemo, useState } from "react";
import type { Condition, LiteralValue, Operator, ValueRef } from "../../../../bindings";
import { useStore } from "../../../../store/useAppStore";
import { getOutputSchema, nodeTypeName } from "../../../../utils/outputSchema";

/** Parse a dotted "node.field" variable name into its parts. */
function parseVariableRef(ref: ValueRef): { node: string; field: string } | null {
  if (ref.type !== "Variable" || !ref.name) return null;
  const dotIdx = ref.name.indexOf(".");
  if (dotIdx === -1) return { node: ref.name, field: "" };
  return { node: ref.name.slice(0, dotIdx), field: ref.name.slice(dotIdx + 1) };
}

/** Build a Variable ValueRef from node auto_id and field name. */
function buildVariableRef(node: string, field: string): ValueRef {
  const name = field ? `${node}.${field}` : node;
  return { type: "Variable", name };
}

function literalDisplayValue(ref: ValueRef): string {
  if (ref.type !== "Literal") return "";
  return String(ref.value.value);
}

interface OutputRefPickerProps {
  nodeAutoId: string;
  fieldName: string;
  onChangeNode: (autoId: string) => void;
  onChangeField: (field: string) => void;
  /** Available upstream nodes with auto_ids and their type names. */
  nodeOptions: Array<{ autoId: string; typeName: string }>;
}

function OutputRefPicker({
  nodeAutoId,
  fieldName,
  onChangeNode,
  onChangeField,
  nodeOptions,
}: OutputRefPickerProps) {
  const selectedTypeName = nodeOptions.find((n) => n.autoId === nodeAutoId)?.typeName ?? "";
  const fields = getOutputSchema(selectedTypeName);

  return (
    <div className="flex gap-1.5">
      <select
        value={nodeAutoId}
        onChange={(e) => {
          onChangeNode(e.target.value);
          // Reset field when node changes
          const newTypeName = nodeOptions.find((n) => n.autoId === e.target.value)?.typeName ?? "";
          const newFields = getOutputSchema(newTypeName);
          if (newFields.length > 0) {
            onChangeField(newFields[0].name);
          } else {
            onChangeField("");
          }
        }}
        className="flex-1 rounded bg-[var(--bg-input)] px-2 py-1 text-xs text-[var(--text-primary)] outline-none"
      >
        <option value="">Select node...</option>
        {nodeOptions.map((opt) => (
          <option key={opt.autoId} value={opt.autoId}>
            {opt.autoId}
          </option>
        ))}
      </select>
      <select
        value={fieldName}
        onChange={(e) => onChangeField(e.target.value)}
        className="flex-1 rounded bg-[var(--bg-input)] px-2 py-1 text-xs text-[var(--text-primary)] outline-none"
        disabled={fields.length === 0}
      >
        {fields.length === 0 ? (
          <option value="">No fields</option>
        ) : (
          fields.map((f) => (
            <option key={f.name} value={f.name}>
              {f.name} ({f.type})
            </option>
          ))
        )}
      </select>
    </div>
  );
}

export function ConditionEditor({
  condition,
  onChange,
}: {
  condition: Condition;
  onChange: (c: Condition) => void;
}) {
  const workflowNodes = useStore((s) => s.workflow.nodes);

  // Build list of available nodes with output schemas
  const nodeOptions = useMemo(() => {
    const options: Array<{ autoId: string; typeName: string }> = [];
    for (const node of workflowNodes) {
      if (!node.auto_id) continue;
      const typeName = nodeTypeName(node.node_type as unknown as Record<string, unknown>);
      const schema = getOutputSchema(typeName);
      if (schema.length > 0) {
        options.push({ autoId: node.auto_id, typeName });
      }
    }
    return options;
  }, [workflowNodes]);

  // Parse left side
  const leftParsed = parseVariableRef(condition.left);
  const leftNode = leftParsed?.node ?? "";
  const leftField = leftParsed?.field ?? "";

  // Right side mode: "Literal" or "Variable"
  const isRightVariable = condition.right.type === "Variable";
  const [rightMode, setRightMode] = useState<"literal" | "variable">(
    isRightVariable ? "variable" : "literal",
  );

  const rightParsed = parseVariableRef(condition.right);
  const rightNode = rightParsed?.node ?? "";
  const rightField = rightParsed?.field ?? "";

  return (
    <div className="space-y-2">
      {/* Left side: OutputRef picker */}
      <div>
        <label className="text-[10px] text-[var(--text-muted)]">Variable</label>
        <OutputRefPicker
          nodeAutoId={leftNode}
          fieldName={leftField}
          onChangeNode={(autoId) => {
            const typeName = nodeOptions.find((n) => n.autoId === autoId)?.typeName ?? "";
            const fields = getOutputSchema(typeName);
            const field = fields.length > 0 ? fields[0].name : "";
            onChange({ ...condition, left: buildVariableRef(autoId, field) });
          }}
          onChangeField={(field) => {
            onChange({ ...condition, left: buildVariableRef(leftNode, field) });
          }}
          nodeOptions={nodeOptions}
        />
      </div>

      {/* Operator */}
      <div className="flex gap-2">
        <div className="flex-1">
          <label className="text-[10px] text-[var(--text-muted)]">Operator</label>
          <select
            value={condition.operator}
            onChange={(e) =>
              onChange({ ...condition, operator: e.target.value as Operator })
            }
            className="w-full rounded bg-[var(--bg-input)] px-2 py-1 text-xs text-[var(--text-primary)] outline-none"
          >
            <option value="Equals">Equals</option>
            <option value="NotEquals">Not Equals</option>
            <option value="GreaterThan">Greater Than</option>
            <option value="LessThan">Less Than</option>
            <option value="GreaterThanOrEqual">&ge;</option>
            <option value="LessThanOrEqual">&le;</option>
            <option value="Contains">Contains</option>
            <option value="NotContains">Not Contains</option>
            <option value="IsEmpty">Is Empty</option>
            <option value="IsNotEmpty">Is Not Empty</option>
          </select>
        </div>
      </div>

      {/* Right side: mode toggle + editor */}
      <div>
        <div className="flex items-center justify-between mb-0.5">
          <label className="text-[10px] text-[var(--text-muted)]">Compare To</label>
          <div className="flex rounded bg-[var(--bg-input)] overflow-hidden">
            <button
              type="button"
              onClick={() => {
                setRightMode("literal");
                if (condition.right.type !== "Literal") {
                  onChange({
                    ...condition,
                    right: { type: "Literal", value: { type: "String", value: "" } },
                  });
                }
              }}
              className={`px-2 py-0.5 text-[10px] transition-colors ${
                rightMode === "literal"
                  ? "bg-[var(--accent-coral)] text-white"
                  : "text-[var(--text-muted)] hover:text-[var(--text-primary)]"
              }`}
            >
              Literal
            </button>
            <button
              type="button"
              onClick={() => {
                setRightMode("variable");
                if (condition.right.type !== "Variable") {
                  onChange({
                    ...condition,
                    right: { type: "Variable", name: "" },
                  });
                }
              }}
              className={`px-2 py-0.5 text-[10px] transition-colors ${
                rightMode === "variable"
                  ? "bg-[var(--accent-coral)] text-white"
                  : "text-[var(--text-muted)] hover:text-[var(--text-primary)]"
              }`}
            >
              Variable
            </button>
          </div>
        </div>
        {rightMode === "literal" ? (
          <input
            type="text"
            value={literalDisplayValue(condition.right)}
            onChange={(e) => {
              const raw = e.target.value;
              let value: LiteralValue;
              if (raw === "true" || raw === "false") {
                value = { type: "Bool", value: raw === "true" };
              } else if (!isNaN(Number(raw)) && raw !== "") {
                value = { type: "Number", value: Number(raw) };
              } else {
                value = { type: "String", value: raw };
              }
              onChange({
                ...condition,
                right: { type: "Literal", value },
              });
            }}
            placeholder="value"
            className="w-full rounded bg-[var(--bg-input)] px-2 py-1 text-xs text-[var(--text-primary)] outline-none"
          />
        ) : (
          <OutputRefPicker
            nodeAutoId={rightNode}
            fieldName={rightField}
            onChangeNode={(autoId) => {
              const typeName = nodeOptions.find((n) => n.autoId === autoId)?.typeName ?? "";
              const fields = getOutputSchema(typeName);
              const field = fields.length > 0 ? fields[0].name : "";
              onChange({ ...condition, right: buildVariableRef(autoId, field) });
            }}
            onChangeField={(field) => {
              onChange({ ...condition, right: buildVariableRef(rightNode, field) });
            }}
            nodeOptions={nodeOptions}
          />
        )}
      </div>
    </div>
  );
}
