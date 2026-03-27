import { typeColor } from "../../utils/typeColors";
import { OUTPUT_SCHEMAS } from "../../utils/outputSchema";

interface OutputsSectionProps {
  nodeTypeName: string;
  autoId?: string;
}

export function OutputsSection({ nodeTypeName, autoId }: OutputsSectionProps) {
  const fields = OUTPUT_SCHEMAS[nodeTypeName];
  if (!fields || fields.length === 0) return null;

  return (
    <div className="mt-3">
      <h4 className="text-xs font-medium text-[var(--text-muted)] mb-1.5">Outputs</h4>
      <div className="space-y-1">
        {fields.map((field) => (
          <div key={field.name} className="flex items-center gap-2 text-xs">
            <span
              className="w-2 h-2 rounded-full flex-shrink-0"
              style={{ backgroundColor: typeColor(field.type) }}
            />
            <span className="font-mono text-[var(--text-primary)]">
              {autoId ? `${autoId}.${field.name}` : field.name}
            </span>
            <span className="text-[var(--text-muted)]">{field.type}</span>
          </div>
        ))}
      </div>
    </div>
  );
}
