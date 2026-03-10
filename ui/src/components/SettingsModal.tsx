import type { EndpointConfig } from "../store/useAppStore";

interface SettingsModalProps {
  open: boolean;
  plannerConfig: EndpointConfig;
  agentConfig: EndpointConfig;
  vlmConfig: EndpointConfig;
  vlmEnabled: boolean;
  mcpCommand: string;
  maxRepairAttempts: number;
  hoverDwellThreshold: number;
  onClose: () => void;
  onPlannerConfigChange: (config: EndpointConfig) => void;
  onAgentConfigChange: (config: EndpointConfig) => void;
  onVlmConfigChange: (config: EndpointConfig) => void;
  onVlmEnabledChange: (enabled: boolean) => void;
  onMcpCommandChange: (cmd: string) => void;
  onMaxRepairAttemptsChange: (n: number) => void;
  onHoverDwellThresholdChange: (ms: number) => void;
}

const inputClass =
  "w-full rounded bg-[var(--bg-input)] px-2.5 py-1.5 text-xs text-[var(--text-primary)] outline-none focus:ring-1 focus:ring-[var(--accent-coral)]";

const endpointFields: {
  key: keyof EndpointConfig;
  label: string;
  type: string;
  placeholder?: string;
}[] = [
  { key: "baseUrl", label: "Base URL", type: "text" },
  { key: "model", label: "Model", type: "text" },
  { key: "apiKey", label: "API Key", type: "password", placeholder: "Optional" },
];

function EndpointFields({
  config,
  onChange,
}: {
  config: EndpointConfig;
  onChange: (config: EndpointConfig) => void;
}) {
  return (
    <div className="space-y-2">
      {endpointFields.map((field) => (
        <div key={field.key}>
          <label className="mb-1 block text-xs text-[var(--text-secondary)]">
            {field.label}
          </label>
          <input
            type={field.type}
            value={config[field.key]}
            onChange={(e) => onChange({ ...config, [field.key]: e.target.value })}
            placeholder={field.placeholder}
            className={`${inputClass}${field.placeholder ? " placeholder-[var(--text-muted)]" : ""}`}
          />
        </div>
      ))}
    </div>
  );
}

function ConfigSection({
  title,
  description,
  config,
  onChange,
}: {
  title: string;
  description: string;
  config: EndpointConfig;
  onChange: (config: EndpointConfig) => void;
}) {
  return (
    <div>
      <h3 className="mb-2 text-xs font-semibold uppercase tracking-wider text-[var(--text-muted)]">
        {title}
      </h3>
      <p className="mb-2 text-[10px] text-[var(--text-muted)]">{description}</p>
      <EndpointFields config={config} onChange={onChange} />
    </div>
  );
}

export function SettingsModal({
  open,
  plannerConfig,
  agentConfig,
  vlmConfig,
  vlmEnabled,
  mcpCommand,
  maxRepairAttempts,
  hoverDwellThreshold,
  onClose,
  onPlannerConfigChange,
  onAgentConfigChange,
  onVlmConfigChange,
  onVlmEnabledChange,
  onMcpCommandChange,
  onMaxRepairAttemptsChange,
  onHoverDwellThresholdChange,
}: SettingsModalProps) {
  if (!open) return null;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
      <div className="w-[480px] max-h-[90vh] overflow-y-auto rounded-lg border border-[var(--border)] bg-[var(--bg-panel)] shadow-xl">
        <div className="flex items-center justify-between border-b border-[var(--border)] px-4 py-3">
          <h2 className="text-sm font-semibold text-[var(--text-primary)]">
            Settings
          </h2>
          <button
            onClick={onClose}
            className="text-[var(--text-muted)] hover:text-[var(--text-primary)]"
          >
            x
          </button>
        </div>

        <div className="space-y-4 p-4">
          <ConfigSection
            title="Planner"
            description="Generates workflows from intent and applies assistant diffs. Typically a larger model."
            config={plannerConfig}
            onChange={onPlannerConfigChange}
          />

          <ConfigSection
            title="Agent"
            description="Powers runtime AI Step nodes with tool access. Only used when workflow contains AI Steps."
            config={agentConfig}
            onChange={onAgentConfigChange}
          />

          <div>
            <div className="mb-2 flex items-center gap-2">
              <h3 className="text-xs font-semibold uppercase tracking-wider text-[var(--text-muted)]">
                Vision (VLM)
              </h3>
              <label className="flex items-center gap-1.5 text-xs text-[var(--text-secondary)] cursor-pointer">
                <input
                  type="checkbox"
                  checked={vlmEnabled}
                  onChange={(e) => onVlmEnabledChange(e.target.checked)}
                  className="accent-[var(--accent-coral)]"
                />
                Separate model
              </label>
            </div>
            {vlmEnabled ? (
              <>
                <p className="mb-2 text-[10px] text-[var(--text-muted)]">
                  Analyzes screenshots and images, returns text summaries to the agent.
                </p>
                <EndpointFields
                  config={vlmConfig}
                  onChange={onVlmConfigChange}
                />
              </>
            ) : (
              <p className="text-[10px] text-[var(--text-muted)]">
                Using agent model for vision. Enable to use a separate vision model.
              </p>
            )}
          </div>

          <div>
            <h3 className="mb-2 text-xs font-semibold uppercase tracking-wider text-[var(--text-muted)]">
              Assistant
            </h3>
            <p className="mb-2 text-[10px] text-[var(--text-muted)]">
              Controls how the assistant validates and retries generated patches.
            </p>
            <div>
              <label className="mb-1 block text-xs text-[var(--text-secondary)]">
                Max repair attempts
              </label>
              <input
                type="number"
                min={0}
                max={10}
                value={maxRepairAttempts}
                onChange={(e) => {
                  const clamped = Math.max(0, Math.min(10, Math.floor(Number(e.target.value) || 0)));
                  onMaxRepairAttemptsChange(clamped);
                }}
                className={inputClass}
              />
              <p className="mt-1 text-[10px] text-[var(--text-muted)]">
                Validate patches and retry on failure. 0 = skip validation, 1 = validate only, 2+ = validate and retry.
              </p>
            </div>
          </div>

          <div>
            <h3 className="mb-2 text-xs font-semibold uppercase tracking-wider text-[var(--text-muted)]">
              native-devtools-mcp
            </h3>
            <p className="mb-2 text-[10px] text-[var(--text-muted)]">
              Provides browser automation and screenshot tools for workflow execution.
            </p>
            <div>
              <label className="mb-1 block text-xs text-[var(--text-secondary)]">
                Binary path
              </label>
              <input
                type="text"
                value={mcpCommand === "npx" ? "" : mcpCommand}
                onChange={(e) =>
                  onMcpCommandChange(e.target.value.trim() || "npx")
                }
                placeholder="Default (npx)"
                className={`${inputClass} placeholder-[var(--text-muted)]`}
              />
              <p className="mt-1 text-[10px] text-[var(--text-muted)]">
                Leave empty to use npx, or set a path to a local binary
              </p>
            </div>
          </div>

          <div>
            <h3 className="mb-2 text-xs font-semibold uppercase tracking-wider text-[var(--text-muted)]">
              Walkthrough
            </h3>
            <p className="mb-2 text-[10px] text-[var(--text-muted)]">
              Controls walkthrough recording behavior.
            </p>
            <div>
              <label className="mb-1 block text-xs text-[var(--text-secondary)]">
                Hover Detection Threshold (ms)
              </label>
              <input
                type="number"
                min={100}
                max={10000}
                value={hoverDwellThreshold}
                onChange={(e) => {
                  const clamped = Math.max(100, Math.min(10000, Math.floor(Number(e.target.value) || 1000)));
                  onHoverDwellThresholdChange(clamped);
                }}
                className={inputClass}
              />
              <p className="mt-1 text-[10px] text-[var(--text-muted)]">
                How long the cursor must stay on an element before it counts as a hover action (100-10000ms).
              </p>
            </div>
          </div>
        </div>

        <div className="flex justify-end border-t border-[var(--border)] px-4 py-3">
          <button
            onClick={onClose}
            className="rounded bg-[var(--accent-coral)] px-4 py-1.5 text-xs font-medium text-white hover:opacity-90"
          >
            Done
          </button>
        </div>
      </div>
    </div>
  );
}
