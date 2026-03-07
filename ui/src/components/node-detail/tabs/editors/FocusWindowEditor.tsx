import type { AppKind, NodeType } from "../../../../bindings";
import { CheckboxField, FieldGroup, SelectField, TextField } from "../../fields";
import { type NodeEditorProps, optionalString } from "./types";

const APP_KIND_LABELS: Record<AppKind, string> = {
  Native: "Native (Accessibility)",
  ChromeBrowser: "Chrome DevTools",
  ElectronApp: "Electron (DevTools)",
};

export function FocusWindowEditor({ nodeType, onUpdate }: NodeEditorProps) {
  const nt = nodeType;
  if (nt.type !== "FocusWindow") return null;

  const updateType = (patch: Record<string, unknown>) => {
    onUpdate({ node_type: { ...nt, ...patch } as NodeType });
  };

  const appKind = nt.app_kind ?? "Native";
  const isCdm = appKind === "ChromeBrowser" || appKind === "ElectronApp";

  return (
    <FieldGroup title="Focus Window">
      <SelectField
        label="Method"
        value={nt.method}
        options={["WindowId", "AppName", "Pid"]}
        onChange={(v) => updateType({ method: v })}
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
      <div>
        <label className="mb-1 block text-xs text-[var(--text-secondary)]">
          Automation
        </label>
        {isCdm ? (
          <select
            value={appKind}
            onChange={(e) => updateType({ app_kind: e.target.value as AppKind })}
            className="w-full rounded bg-[var(--bg-input)] px-2.5 py-1.5 text-xs text-[var(--text-primary)] outline-none focus:ring-1 focus:ring-[var(--accent-coral)]"
          >
            <option value={appKind}>{APP_KIND_LABELS[appKind]}</option>
            <option value="Native">{APP_KIND_LABELS.Native}</option>
          </select>
        ) : (
          <span className="block px-2.5 py-1.5 text-xs text-[var(--text-muted)]">
            {APP_KIND_LABELS.Native}
          </span>
        )}
      </div>
      {appKind === "ElectronApp" && (
        <p className="mt-1 text-[10px] text-[var(--text-muted)]">
          App will be restarted with DevTools enabled on first run.
        </p>
      )}
    </FieldGroup>
  );
}
