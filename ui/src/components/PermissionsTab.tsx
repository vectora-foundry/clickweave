import { useEffect, useState } from "react";
import type { ConfirmableTool } from "../bindings";
import { commands } from "../bindings";
import { DEFAULT_TOOL_PERMISSIONS } from "../store/state";
import type { PermissionLevel, PermissionRule, ToolPermissions } from "../store/state";

const inputClass =
  "bg-[var(--bg-input)] text-[var(--text-primary)] border border-[var(--border)] rounded-md px-2.5 py-1 text-[11px] cursor-pointer";

const textInputClass =
  "bg-[var(--bg-input)] text-[var(--text-primary)] border border-[var(--border)] rounded-md px-2.5 py-1 text-[11px] font-mono flex-1 min-w-0";

const settingRowClass =
  "flex items-center justify-between gap-3 rounded-lg bg-[var(--bg-dark)] px-3.5 py-2.5";

interface SettingRowProps {
  title: string;
  description: string;
  control: React.ReactNode;
}

function SettingRow({ title, description, control }: SettingRowProps) {
  return (
    <div className={settingRowClass}>
      <div>
        <div className="text-xs font-semibold text-[var(--text-primary)]">
          {title}
        </div>
        <div className="mt-0.5 text-[10px] text-[var(--text-muted)]">
          {description}
        </div>
      </div>
      {control}
    </div>
  );
}

interface ToggleProps {
  checked: boolean;
  onChange: (next: boolean) => void;
}

function Toggle({ checked, onChange }: ToggleProps) {
  return (
    <button
      role="switch"
      aria-checked={checked}
      onClick={() => onChange(!checked)}
      className={`relative h-[22px] w-10 flex-shrink-0 rounded-full transition-colors ${
        checked ? "bg-[var(--accent-coral)]" : "bg-[var(--bg-input)]"
      }`}
    >
      <span
        className={`absolute top-[3px] h-4 w-4 rounded-full bg-white transition-[left] ${
          checked ? "left-[21px]" : "left-[3px]"
        }`}
      />
    </button>
  );
}

interface PermissionsTabProps {
  toolPermissions: ToolPermissions;
  onToolPermissionsChange: (perms: ToolPermissions) => void;
  onToolPermissionChange: (toolName: string, level: PermissionLevel) => void;
}

/** Clamp an unknown numeric input to the 0..20 range the cap accepts. */
function clampCap(raw: unknown): number {
  const n = Number(raw);
  if (!Number.isFinite(n)) return DEFAULT_TOOL_PERMISSIONS.consecutiveDestructiveCap;
  return Math.max(0, Math.min(20, Math.floor(n)));
}

export function PermissionsTab({
  toolPermissions,
  onToolPermissionsChange,
  onToolPermissionChange,
}: PermissionsTabProps) {
  const {
    allowAll,
    tools,
    patternRules,
    requireConfirmDestructive,
    consecutiveDestructiveCap,
  } = toolPermissions;
  const [confirmableTools, setConfirmableTools] = useState<ConfirmableTool[]>([]);

  useEffect(() => {
    commands.confirmableTools().then(setConfirmableTools);
  }, []);

  const updateRule = (index: number, patch: Partial<PermissionRule>) => {
    const next = patternRules.map((r, i) => (i === index ? { ...r, ...patch } : r));
    onToolPermissionsChange({ ...toolPermissions, patternRules: next });
  };

  const removeRule = (index: number) => {
    const next = patternRules.filter((_, i) => i !== index);
    onToolPermissionsChange({ ...toolPermissions, patternRules: next });
  };

  const addRule = () => {
    const next: PermissionRule[] = [
      ...patternRules,
      { toolPattern: "*", argsPattern: "", action: "ask" },
    ];
    onToolPermissionsChange({ ...toolPermissions, patternRules: next });
  };

  return (
    <div className="space-y-4 p-4">
      <SettingRow
        title="Require confirmation for destructive actions even when allowed"
        description="Destructive tools (send, submit, delete) will still prompt even if they or the global override are set to allow."
        control={
          <Toggle
            checked={requireConfirmDestructive}
            onChange={(next) =>
              onToolPermissionsChange({
                ...toolPermissions,
                requireConfirmDestructive: next,
              })
            }
          />
        }
      />

      <SettingRow
        title="Allow all planning actions"
        description="Skip confirmation for all tools. Per-tool settings are ignored, but the destructive guardrail above still applies when it is on."
        control={
          <Toggle
            checked={allowAll}
            onChange={(next) =>
              onToolPermissionsChange({ ...toolPermissions, allowAll: next })
            }
          />
        }
      />

      <SettingRow
        title="Consecutive destructive call cap"
        description="Halt the run after this many destructive actions in a row. 0 disables the cap."
        control={
          <input
            type="number"
            min={0}
            max={20}
            value={consecutiveDestructiveCap}
            onChange={(e) =>
              onToolPermissionsChange({
                ...toolPermissions,
                consecutiveDestructiveCap: clampCap(e.target.value),
              })
            }
            className={`${inputClass} w-16 text-center`}
          />
        }
      />

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
                    e.target.value as PermissionLevel,
                  )
                }
                disabled={allowAll}
                className={inputClass}
              >
                <option value="ask">Ask every time</option>
                <option value="allow">Always allow</option>
                <option value="deny">Always deny</option>
              </select>
            </div>
          ))}
        </div>
      </div>

      {/* Pattern rules */}
      <div>
        <h3 className="mb-2 text-[10px] font-semibold uppercase tracking-wider text-[var(--text-muted)]">
          Pattern rules
        </h3>
        <div className="mb-2 text-[10px] text-[var(--text-muted)]">
          Rules apply to any tool matching the glob. Deny beats Ask beats Allow
          when multiple rules match. The args substring is optional.
        </div>
        <div className="space-y-1.5">
          {patternRules.map((rule, index) => (
            <div
              key={index}
              className="flex items-center gap-2 rounded-lg bg-[var(--bg-dark)] px-3 py-2"
            >
              <input
                type="text"
                value={rule.toolPattern}
                onChange={(e) =>
                  updateRule(index, { toolPattern: e.target.value })
                }
                placeholder="cdp_*"
                aria-label="Tool glob pattern"
                className={textInputClass}
              />
              <input
                type="text"
                value={rule.argsPattern ?? ""}
                onChange={(e) =>
                  updateRule(index, { argsPattern: e.target.value })
                }
                placeholder="args substring (optional)"
                aria-label="Args substring"
                className={textInputClass}
              />
              <select
                value={rule.action}
                onChange={(e) =>
                  updateRule(index, { action: e.target.value as PermissionLevel })
                }
                className={inputClass}
                aria-label="Rule action"
              >
                <option value="allow">Allow</option>
                <option value="ask">Ask</option>
                <option value="deny">Deny</option>
              </select>
              <button
                type="button"
                onClick={() => removeRule(index)}
                aria-label="Remove rule"
                className="flex-shrink-0 rounded-md border border-[var(--border)] px-2 py-1 text-[11px] text-[var(--text-secondary)] hover:text-[var(--text-primary)]"
              >
                &times;
              </button>
            </div>
          ))}
          <button
            type="button"
            onClick={addRule}
            className="rounded-md border border-[var(--border)] px-3 py-1.5 text-[11px] text-[var(--text-secondary)] hover:text-[var(--text-primary)]"
          >
            + Add rule
          </button>
        </div>
      </div>

      {/* Reset */}
      <div className="flex items-baseline gap-2.5 border-t border-[var(--border)] pt-3.5">
        <button
          onClick={() => onToolPermissionsChange(DEFAULT_TOOL_PERMISSIONS)}
          className="flex-shrink-0 rounded-md border border-[var(--border)] px-3 py-1.5 text-[11px] text-[var(--text-secondary)] hover:text-[var(--text-primary)]"
        >
          Reset all to &quot;Ask every time&quot;
        </button>
        <span className="text-[10px] text-[var(--text-muted)]">
          Revokes all saved permissions and rules.
        </span>
      </div>
    </div>
  );
}
