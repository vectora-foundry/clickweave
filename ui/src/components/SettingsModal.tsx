import { useEffect, useRef, useState } from "react";
import type { ChromeProfile } from "../bindings";
import { commands } from "../bindings";
import type { EndpointConfig } from "../store/useAppStore";
import { Modal } from "./Modal";

interface SettingsModalProps {
  open: boolean;
  plannerConfig: EndpointConfig;
  agentConfig: EndpointConfig;
  vlmConfig: EndpointConfig;
  vlmEnabled: boolean;
  maxRepairAttempts: number;
  hoverDwellThreshold: number;
  selectedChromeProfileId: string | null;
  onClose: () => void;
  onPlannerConfigChange: (config: EndpointConfig) => void;
  onAgentConfigChange: (config: EndpointConfig) => void;
  onVlmConfigChange: (config: EndpointConfig) => void;
  onVlmEnabledChange: (enabled: boolean) => void;
  onMaxRepairAttemptsChange: (n: number) => void;
  onHoverDwellThresholdChange: (ms: number) => void;
  onSelectedChromeProfileIdChange: (id: string) => void;
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

function ChromeProfileSection({
  selectedProfileId,
  onProfileChange,
}: {
  selectedProfileId: string | null;
  onProfileChange: (id: string) => void;
}) {
  const [profiles, setProfiles] = useState<ChromeProfile[]>([]);
  const [loading, setLoading] = useState(false);
  const [newName, setNewName] = useState("");
  const [showNewInput, setShowNewInput] = useState(false);
  const selectedProfileIdRef = useRef(selectedProfileId);
  selectedProfileIdRef.current = selectedProfileId;
  const onProfileChangeRef = useRef(onProfileChange);
  onProfileChangeRef.current = onProfileChange;

  const fetchProfiles = async () => {
    setLoading(true);
    const result = await commands.listChromeProfiles();
    if (result.status === "ok") {
      setProfiles(result.data);
      if (!selectedProfileIdRef.current && result.data.length > 0) {
        onProfileChangeRef.current(result.data[0].id);
      }
    }
    setLoading(false);
  };

  useEffect(() => {
    fetchProfiles();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const handleCreate = async () => {
    if (!newName.trim()) return;
    const result = await commands.createChromeProfile(newName.trim());
    if (result.status === "ok") {
      onProfileChange(result.data.id);
      setNewName("");
      setShowNewInput(false);
      fetchProfiles();
    }
  };

  const handleConfigure = async () => {
    if (!selectedProfileId) return;
    await commands.launchChromeForSetup(selectedProfileId);
  };

  return (
    <div>
      <h3 className="mb-2 text-xs font-semibold uppercase tracking-wider text-[var(--text-muted)]">
        Chrome Profile
      </h3>
      <p className="mb-2 text-[10px] text-[var(--text-muted)]">
        Persistent browser profile for Chrome sessions. Log in once, stay logged in across all runs.
      </p>
      <div className="flex items-center gap-2">
        <select
          value={selectedProfileId ?? ""}
          onChange={(e) => onProfileChange(e.target.value)}
          disabled={loading || profiles.length === 0}
          className={inputClass}
        >
          {profiles.map((p) => (
            <option key={p.id} value={p.id}>
              {p.name}
            </option>
          ))}
        </select>
        <button
          onClick={() => setShowNewInput(!showNewInput)}
          className="shrink-0 rounded bg-[var(--bg-input)] px-2 py-1.5 text-xs text-[var(--text-secondary)] hover:text-[var(--text-primary)]"
          title="New profile"
        >
          +
        </button>
      </div>
      {showNewInput && (
        <div className="mt-2 flex items-center gap-2">
          <input
            type="text"
            value={newName}
            onChange={(e) => setNewName(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && handleCreate()}
            placeholder="Profile name"
            autoFocus
            className={`${inputClass} placeholder-[var(--text-muted)]`}
          />
          <button
            onClick={handleCreate}
            disabled={!newName.trim()}
            className="shrink-0 rounded bg-[var(--accent-coral)] px-2.5 py-1.5 text-xs font-medium text-white hover:opacity-90 disabled:opacity-50"
          >
            Create
          </button>
        </div>
      )}
      <button
        onClick={handleConfigure}
        disabled={!selectedProfileId}
        className="mt-2 rounded bg-[var(--bg-input)] px-3 py-1.5 text-xs text-[var(--text-secondary)] hover:text-[var(--text-primary)] disabled:opacity-50"
      >
        Configure (opens Chrome)
      </button>
      <p className="mt-1 text-[10px] text-[var(--text-muted)]">
        Opens Chrome with this profile so you can log into sites. Close Chrome when done.
      </p>
    </div>
  );
}

export function SettingsModal({
  open,
  plannerConfig,
  agentConfig,
  vlmConfig,
  vlmEnabled,
  maxRepairAttempts,
  hoverDwellThreshold,
  selectedChromeProfileId,
  onClose,
  onPlannerConfigChange,
  onAgentConfigChange,
  onVlmConfigChange,
  onVlmEnabledChange,
  onMaxRepairAttemptsChange,
  onHoverDwellThresholdChange,
  onSelectedChromeProfileIdChange,
}: SettingsModalProps) {
  return (
    <Modal open={open} onClose={onClose} className="w-[480px] max-h-[90vh] overflow-y-auto rounded-lg border border-[var(--border)] bg-[var(--bg-panel)] shadow-xl">
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

          <ChromeProfileSection
            selectedProfileId={selectedChromeProfileId}
            onProfileChange={onSelectedChromeProfileIdChange}
          />

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
    </Modal>
  );
}
