import { FieldGroup, ImagePathField, NumberField } from "../../fields";
import { type NodeEditorProps, optionalString } from "./types";
import { useNodeTypeUpdater } from "./useNodeTypeUpdater";

export function FindImageEditor({ nodeType, onUpdate, projectPath }: NodeEditorProps) {
  const nt = nodeType;
  if (nt.type !== "FindImage") return null;

  const updateType = useNodeTypeUpdater(nt, onUpdate);

  return (
    <FieldGroup title="Find Image">
      <ImagePathField
        label="Template Image"
        value={nt.template_image ?? ""}
        projectPath={projectPath}
        onChange={(v) => updateType({ template_image: optionalString(v) })}
      />
      <NumberField
        label="Threshold"
        value={nt.threshold}
        min={0}
        max={1}
        step={0.01}
        onChange={(v) => updateType({ threshold: v })}
      />
      <NumberField
        label="Max Results"
        value={nt.max_results}
        min={1}
        max={20}
        onChange={(v) => updateType({ max_results: v })}
      />
    </FieldGroup>
  );
}
