/**
 * `RunWithValuesForm` — modal form that collects runtime variable values
 * before dispatching a skill run.
 *
 * When a skill has variables whose `default` is null (required), the user
 * must supply values before the run can start. If all variables have
 * defaults the form is bypassed entirely (the caller should not mount this
 * component in that case — see `SkillView`).
 *
 * Chat-driven invocation skips this form: the assistant parses variable
 * values from the natural-language message and calls `runSkillFromView`
 * directly with the parsed bindings.
 */

import { useState } from "react";
import type { JsonValue, Skill, SkillFrontmatterVariable } from "../../bindings";

interface RunWithValuesFormProps {
  skill: Skill;
  onSubmit: (variables: Record<string, JsonValue>) => void;
  onCancel: () => void;
}

export function RunWithValuesForm({ skill, onSubmit, onCancel }: RunWithValuesFormProps) {
  const variables: SkillFrontmatterVariable[] = skill.variables ?? [];

  // Initialize form state: required vars start empty, optional vars start with their default.
  const [values, setValues] = useState<Record<string, string>>(() => {
    const init: Record<string, string> = {};
    for (const v of variables) {
      init[v.name] = v.default != null ? String(v.default) : "";
    }
    return init;
  });

  const requiredVars = variables.filter((v) => v.default === null || v.default === undefined);
  const allRequiredFilled = requiredVars.every((v) => values[v.name]?.trim() !== "");

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault();
    if (!allRequiredFilled) return;
    // Build the variables map — coerce to the variable's declared type where possible.
    const result: Record<string, JsonValue> = {};
    for (const v of variables) {
      const raw = values[v.name] ?? "";
      if (v.type === "number") {
        result[v.name] = Number(raw);
      } else if (v.type === "boolean") {
        result[v.name] = raw.toLowerCase() === "true";
      } else {
        result[v.name] = raw;
      }
    }
    onSubmit(result);
  };

  if (variables.length === 0) {
    // No variables — run immediately. This should not happen if the caller
    // checks correctly, but handle it gracefully.
    onSubmit({});
    return null;
  }

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/50"
      onClick={onCancel}
    >
      <div
        className="mx-4 w-full max-w-md rounded-lg border border-[var(--border)] bg-[var(--bg-panel)] p-5 shadow-xl"
        onClick={(e) => e.stopPropagation()}
      >
        <h3 className="mb-1 text-sm font-semibold text-[var(--text-primary)]">
          Run: {skill.name}
        </h3>
        <p className="mb-4 text-[11px] text-[var(--text-secondary)]">
          Fill in the required values to start the run.
        </p>

        <form onSubmit={handleSubmit} data-testid="run-with-values-form">
          <div className="space-y-3">
            {variables.map((v) => {
              const isRequired = v.default === null || v.default === undefined;
              return (
                <div key={v.name}>
                  <label className="mb-1 block text-[11px] font-medium text-[var(--text-primary)]">
                    {v.name}
                    {isRequired && (
                      <span className="ml-1 text-red-400" aria-label="required">*</span>
                    )}
                    {v.description && (
                      <span className="ml-1 font-normal text-[var(--text-muted)]">
                        — {v.description}
                      </span>
                    )}
                  </label>
                  <input
                    type={v.type === "number" ? "number" : "text"}
                    required={isRequired}
                    data-testid={`var-input-${v.name}`}
                    value={values[v.name] ?? ""}
                    onChange={(e) =>
                      setValues((prev) => ({ ...prev, [v.name]: e.target.value }))
                    }
                    placeholder={v.default != null ? String(v.default) : `Enter ${v.name}…`}
                    className="w-full rounded border border-[var(--border)] bg-[var(--bg-input)] px-3 py-1.5 text-sm text-[var(--text-primary)] outline-none placeholder:text-[var(--text-muted)] focus:border-[var(--accent-coral)]"
                  />
                </div>
              );
            })}
          </div>

          <div className="mt-5 flex justify-end gap-2">
            <button
              type="button"
              onClick={onCancel}
              className="rounded border border-[var(--border)] px-3 py-1.5 text-xs text-[var(--text-secondary)] hover:bg-[var(--bg-hover)]"
            >
              Cancel
            </button>
            <button
              type="submit"
              disabled={!allRequiredFilled}
              data-testid="run-with-values-submit"
              className="rounded bg-[var(--accent-coral)] px-3 py-1.5 text-xs font-medium text-white hover:opacity-90 disabled:opacity-40"
            >
              Run
            </button>
          </div>
        </form>
      </div>
    </div>
  );
}
