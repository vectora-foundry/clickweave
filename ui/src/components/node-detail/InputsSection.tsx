import { typeColor } from "../../utils/typeColors";
import { extractOutputRefs } from "../../utils/outputSchema";

interface InputsSectionProps {
  nodeType: Record<string, unknown>;
}

const REF_FIELD_LABELS: Record<string, string> = {
  target_ref: "Target coordinates",
  text_ref: "Text value",
  value_ref: "Value",
  url_ref: "URL",
  from_ref: "Start coordinates",
  to_ref: "End coordinates",
  prompt_ref: "Prompt data",
};

const REF_TYPE_HINTS: Record<string, string> = {
  target_ref: "Object",
  from_ref: "Object",
  to_ref: "Object",
  text_ref: "String",
  value_ref: "String",
  url_ref: "String",
  prompt_ref: "String",
};

export function InputsSection({ nodeType }: InputsSectionProps) {
  const refs = extractOutputRefs(nodeType).map(({ key, ref }) => ({
    key,
    label: REF_FIELD_LABELS[key] || key,
    ref,
  }));

  if (refs.length === 0) return null;

  return (
    <div className="mt-3">
      <h4 className="text-xs font-medium text-[var(--text-muted)] mb-1.5">Inputs</h4>
      <div className="space-y-1">
        {refs.map(({ key, label, ref }) => (
          <div key={key} className="flex items-center gap-2 text-xs">
            <span
              className="w-2 h-2 rounded-full flex-shrink-0"
              style={{ backgroundColor: typeColor(REF_TYPE_HINTS[key] || "Any") }}
            />
            <span className="text-[var(--text-muted)]">{label}:</span>
            <span className="font-mono text-[var(--text-primary)]">
              {ref.node}.{ref.field}
            </span>
          </div>
        ))}
      </div>
    </div>
  );
}
