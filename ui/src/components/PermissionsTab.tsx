import { useEffect, useState } from "react";
import type { ConfirmableTool } from "../bindings";
import { commands } from "../bindings";
import { DEFAULT_TOOL_PERMISSIONS } from "../store/state";
import type { ToolPermissions } from "../store/state";

const inputClass =
  "bg-[var(--bg-input)] text-[var(--text-primary)] border border-[var(--border)] rounded-md px-2.5 py-1 text-[11px] cursor-pointer";

interface PermissionsTabProps {
  toolPermissions: ToolPermissions;
  onToolPermissionsChange: (perms: ToolPermissions) => void;
  onToolPermissionChange: (toolName: string, level: "ask" | "allow") => void;
}

export function PermissionsTab({
  toolPermissions,
  onToolPermissionsChange,
  onToolPermissionChange,
}: PermissionsTabProps) {
  const { allowAll, tools } = toolPermissions;
  const [confirmableTools, setConfirmableTools] = useState<ConfirmableTool[]>([]);

  useEffect(() => {
    commands.confirmableTools().then(setConfirmableTools);
  }, []);

  return (
    <div className="space-y-4 p-4">
      {/* Global override */}
      <div className="flex items-center justify-between gap-3 rounded-lg bg-[var(--bg-dark)] px-3.5 py-2.5">
        <div>
          <div className="text-xs font-semibold text-[var(--text-primary)]">
            Allow all planning actions
          </div>
          <div className="mt-0.5 text-[10px] text-[var(--text-muted)]">
            Skip confirmation for all tools. Per-tool settings are ignored.
          </div>
        </div>
        <button
          role="switch"
          aria-checked={allowAll}
          onClick={() =>
            onToolPermissionsChange({ ...toolPermissions, allowAll: !allowAll })
          }
          className={`relative h-[22px] w-10 flex-shrink-0 rounded-full transition-colors ${
            allowAll ? "bg-[var(--accent-coral)]" : "bg-[var(--bg-input)]"
          }`}
        >
          <span
            className={`absolute top-[3px] h-4 w-4 rounded-full bg-white transition-[left] ${
              allowAll ? "left-[21px]" : "left-[3px]"
            }`}
          />
        </button>
      </div>

      {/* Per-tool section */}
      <div>
        <h3 className="mb-2 text-[10px] font-semibold uppercase tracking-wider text-[var(--text-muted)]">
          Per-tool permissions
        </h3>
        <div className="space-y-1.5">
          {confirmableTools.map((tool) => (
            <div
              key={tool.name}
              className={`flex items-center justify-between gap-3 rounded-lg bg-[var(--bg-dark)] px-3.5 py-2 ${
                allowAll ? "opacity-40 pointer-events-none" : ""
              }`}
            >
              <div className="min-w-0 flex-1">
                <div className="font-mono text-xs text-[var(--text-primary)]">
                  {tool.name}
                </div>
                <div className="mt-0.5 text-[10px] text-[var(--text-muted)]">
                  {tool.description}
                </div>
              </div>
              <select
                value={tools[tool.name] ?? "ask"}
                onChange={(e) =>
                  onToolPermissionChange(
                    tool.name,
                    e.target.value as "ask" | "allow",
                  )
                }
                disabled={allowAll}
                className={inputClass}
              >
                <option value="ask">Ask every time</option>
                <option value="allow">Always allow</option>
              </select>
            </div>
          ))}
        </div>
      </div>

      {/* Reset */}
      <div className="flex items-baseline gap-2.5 border-t border-[var(--border)] pt-3.5">
        <button
          onClick={() =>
            onToolPermissionsChange(DEFAULT_TOOL_PERMISSIONS)
          }
          className="flex-shrink-0 rounded-md border border-[var(--border)] px-3 py-1.5 text-[11px] text-[var(--text-secondary)] hover:text-[var(--text-primary)]"
        >
          Reset all to &quot;Ask every time&quot;
        </button>
        <span className="text-[10px] text-[var(--text-muted)]">
          Revokes all saved permissions.
        </span>
      </div>
    </div>
  );
}
