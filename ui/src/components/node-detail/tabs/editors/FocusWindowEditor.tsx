import type { AppKind } from "../../../../bindings";
import { CheckboxField, FieldGroup, SelectField, TextField } from "../../fields";
import { APP_KIND_LABELS, type NodeEditorProps, optionalString, usesCdp } from "./types";
import { useNodeTypeUpdater } from "./useNodeTypeUpdater";

export function FocusWindowEditor({ nodeType, onUpdate }: NodeEditorProps) {
  const nt = nodeType;
  if (nt.type !== "FocusWindow") return null;

  const updateType = useNodeTypeUpdater(nt, onUpdate);

  const appKind = nt.app_kind ?? "Native";

  return (
    <FieldGroup title="Focus Window">
      <SelectField
        label="Method"
        value={nt.method}
        options={["WindowId", "AppName", "Pid"]}
        onChange={(v) => {
          // Clear app_kind when switching away from AppName since CDP
          // is only supported for the AppName method.
          const patch: Record<string, unknown> = { method: v };
          if (v !== "AppName" && usesCdp(appKind)) {
            patch.app_kind = "Native";
          }
          updateType(patch);
        }}
      />
      <TextField
        label={
          { WindowId: "Window ID", AppName: "App Name", Pid: "Process ID" }[nt.method] ?? nt.method
        }
        value={nt.value ?? ""}
        onChange={(v) => updateType({ value: optionalString(v) })}
      />
      <CheckboxField
        label="Bring to Front"
        value={nt.bring_to_front}
        onChange={(v) => updateType({ bring_to_front: v })}
      />
      {nt.method === "AppName" && (
        <>
          <SelectField
            label="Automation"
            value={appKind}
            options={Object.keys(APP_KIND_LABELS) as AppKind[]}
            labels={APP_KIND_LABELS}
            onChange={(v) => updateType({ app_kind: v as AppKind })}
          />
          {usesCdp(appKind) && (
            <p className="mt-1 text-[10px] text-[var(--text-muted)]">
              App will be restarted with DevTools enabled on first run.
            </p>
          )}
        </>
      )}
    </FieldGroup>
  );
}
