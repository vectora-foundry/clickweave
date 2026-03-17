import { FieldGroup, TextAreaField, TextField } from "../../fields";
import type { NodeEditorProps } from "./types";
import { useNodeTypeUpdater } from "./useNodeTypeUpdater";

export function McpToolCallEditor({ nodeType, onUpdate }: NodeEditorProps) {
  const nt = nodeType;
  if (nt.type !== "McpToolCall") return null;

  const updateType = useNodeTypeUpdater(nt, onUpdate);

  return (
    <FieldGroup title="MCP Tool Call">
      <TextField
        label="Tool Name"
        value={nt.tool_name}
        onChange={(v) => updateType({ tool_name: v })}
      />
      <TextAreaField
        label="Arguments (JSON)"
        value={
          typeof nt.arguments === "string"
            ? nt.arguments
            : JSON.stringify(nt.arguments ?? {}, null, 2)
        }
        onChange={(v) => {
          try {
            updateType({ arguments: JSON.parse(v) });
          } catch {
            // keep raw string while user is editing
          }
        }}
      />
    </FieldGroup>
  );
}
