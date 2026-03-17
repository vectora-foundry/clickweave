import {
  FieldGroup,
  ImagePathField,
  NumberField,
  TextAreaField,
  TextField,
} from "../../fields";
import { type NodeEditorProps, optionalString } from "./types";
import { useNodeTypeUpdater } from "./useNodeTypeUpdater";

export function AiStepEditor({ nodeType, onUpdate, projectPath }: NodeEditorProps) {
  const nt = nodeType;
  if (nt.type !== "AiStep") return null;

  const updateType = useNodeTypeUpdater(nt, onUpdate);

  return (
    <FieldGroup title="AI Step">
      <TextAreaField
        label="Prompt"
        value={nt.prompt}
        onChange={(prompt) => updateType({ prompt })}
      />
      <TextField
        label="Button Text"
        value={nt.button_text ?? ""}
        onChange={(v) => updateType({ button_text: optionalString(v) })}
        placeholder="Optional"
      />
      <ImagePathField
        label="Template Image"
        value={nt.template_image ?? ""}
        projectPath={projectPath}
        onChange={(v) => updateType({ template_image: optionalString(v) })}
      />
      <NumberField
        label="Max Tool Calls"
        value={nt.max_tool_calls ?? 10}
        min={1}
        max={100}
        onChange={(v) => updateType({ max_tool_calls: v })}
      />
      <TextField
        label="Allowed Tools"
        value={nt.allowed_tools?.join(", ") ?? ""}
        onChange={(v) =>
          updateType({
            allowed_tools: v === "" ? null : v.split(",").map((s) => s.trim()),
          })
        }
        placeholder="Comma-separated, blank = all"
      />
    </FieldGroup>
  );
}
