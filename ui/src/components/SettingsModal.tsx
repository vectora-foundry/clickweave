import { useCallback, useEffect, useRef, useState } from "react";
import type { ChromeProfile } from "../bindings";
import { commands } from "../bindings";
import type { EndpointConfig } from "../store/useAppStore";
import type { PermissionLevel, ToolPermissions } from "../store/state";
import { Modal } from "./Modal";
import { ExecutionTab } from "./ExecutionTab";
import { PermissionsTab } from "./PermissionsTab";
import { PrivacyTab } from "./PrivacyTab";

type HealthState = "idle" | "pending" | "checking" | "ok" | "error";

function EndpointStatus({ baseUrl, apiKey, model }: { baseUrl: string; apiKey?: string; model?: string }) {
  const [state, setState] = useState<HealthState>("idle");
  const [error, setError] = useState<string | null>(null);
  const debounceRef = useRef<ReturnType<typeof setTimeout>>(undefined);

  useEffect(() => {
    if (!baseUrl || baseUrl.trim() === "") {
      setState("idle");
      return;
    }

    let cancelled = false;
    setState("pending");
    clearTimeout(debounceRef.current);
    debounceRef.current = setTimeout(async () => {
      setState("checking");
      const result = await commands.checkEndpoint(baseUrl, apiKey || null, model || null);
      if (cancelled) return;
      if (result.status === "ok") {
        setState("ok");
        setError(null);
      } else {
        setState("error");
        setError(result.error.message ?? "Unreachable");
      }
    }, 500);

    return () => {
      cancelled = true;
      clearTimeout(debounceRef.current);
    };
  }, [baseUrl, apiKey, model]);

  if (state === "idle") return null;
  if (state === "pending") return null;
  if (state === "checking") return <span className="ml-2 text-gray-400 text-xs">checking...</span>;
  if (state === "ok") return <span className="ml-2 text-green-500 text-xs">●</span>;
  return (
    <span className="ml-2 text-red-500 text-xs" title={error ?? "Unreachable"}>
      ●
    </span>
  );
}

type SettingsTab = "general" | "execution" | "permissions" | "privacy";

interface SettingsModalProps {
  open: boolean;
  supervisorConfig: EndpointConfig;
  agentConfig: EndpointConfig;
  fastConfig: EndpointConfig;
  fastEnabled: boolean;
  maxRepairAttempts: number;
  hoverDwellThreshold: number;
  supervisionDelayMs: number;
  toolPermissions: ToolPermissions;
  traceRetentionDays: number;
  storeTraces: boolean;
  episodicEnabled: boolean;
  retrievedEpisodesK: number;
  episodicGlobalParticipation: boolean;
  onClose: () => void;
  onSupervisorConfigChange: (config: EndpointConfig) => void;
  onAgentConfigChange: (config: EndpointConfig) => void;
  onFastConfigChange: (config: EndpointConfig) => void;
  onFastEnabledChange: (enabled: boolean) => void;
  onMaxRepairAttemptsChange: (n: number) => void;
  onHoverDwellThresholdChange: (ms: number) => void;
  onSupervisionDelayMsChange: (ms: number) => void;
  onToolPermissionsChange: (perms: ToolPermissions) => void;
  onToolPermissionChange: (toolName: string, level: PermissionLevel) => void;
  onTraceRetentionDaysChange: (days: number) => void;
  onStoreTracesChange: (enabled: boolean) => void;
  onEpisodicEnabledChange: (enabled: boolean) => void;
  onRetrievedEpisodesKChange: (n: number) => void;
  onEpisodicGlobalParticipationChange: (enabled: boolean) => void;
}

const inputClass =
  "w-full rounded bg-[var(--bg-input)] px-2.5 py-1.5 text-xs text-[var(--text-primary)] outline-none focus:ring-1 focus:ring-[var(--accent-coral)]";

/**
 * Pure helper — determines which model to select after a fresh model list
 * fetch. Exported for unit testing.
 *
 * Rules (in priority order):
 * 1. If `currentModel` exactly matches an entry in `models`, keep it.
 * 2. If `currentModel` matches an entry fuzzily (equal after stripping
 *    `.gguf` / `.bin` suffixes, or either side `endsWith` the other's
 *    bare form), canonicalize to the server's id. This mirrors the
 *    backend `check_endpoint` acceptance rules so the stored config is
 *    not rewritten to a different quantization — but we use the server's
 *    string as the actual value, so the rendered `<select>` has a
 *    matching `<option>` and the control does not go blank.
 * 3. If there is exactly one model, select it.
 * 4. Otherwise select the first model and return a note so the caller can
 *    surface it to the user.
 *
 * Returns `{ model, note }` where `note` is non-null only when the current
 * model was not found and the list had multiple entries.
 */
export function selectModel(
  models: string[],
  currentModel: string,
): { model: string; note: string | null } {
  if (models.length === 0) {
    return { model: currentModel, note: null };
  }
  if (models.includes(currentModel)) {
    return { model: currentModel, note: null };
  }
  const stripExt = (s: string) =>
    s.endsWith(".gguf") ? s.slice(0, -".gguf".length)
    : s.endsWith(".bin") ? s.slice(0, -".bin".length)
    : s;
  const currentBare = stripExt(currentModel);
  const fuzzyMatch = models.find((m) => {
    const bare = stripExt(m);
    return bare === currentBare || bare.endsWith(currentBare) || currentBare.endsWith(bare);
  });
  if (fuzzyMatch !== undefined) {
    return { model: fuzzyMatch, note: null };
  }
  if (models.length === 1) {
    return { model: models[0], note: null };
  }
  return {
    model: models[0],
    note: `model updated to first available: ${models[0]}`,
  };
}

type ModelFetchState =
  | { status: "idle" }
  | { status: "loading" }
  | { status: "ok"; models: string[] }
  | { status: "error"; error: string; models: string[] };

function ModelDropdown({
  config,
  onChange,
}: {
  config: EndpointConfig;
  onChange: (config: EndpointConfig) => void;
}) {
  const [fetchState, setFetchState] = useState<ModelFetchState>({ status: "idle" });
  const [note, setNote] = useState<string | null>(null);
  const debounceRef = useRef<ReturnType<typeof setTimeout>>(undefined);

  // Generation counter invalidates in-flight /models responses from a
  // previous (baseUrl, apiKey) pair. Bumped in the effect on every
  // endpoint-field change AND in the explicit refresh button, so late
  // responses from a prior endpoint (including one in flight during the
  // 400ms debounce or after baseUrl was cleared) cannot write stale
  // fields back into the store.
  const fetchGenRef = useRef(0);

  const fetchModels = useCallback(
    async (cfg: EndpointConfig, gen: number) => {
      if (!cfg.baseUrl.trim()) {
        setFetchState({ status: "idle" });
        return;
      }
      setFetchState({ status: "loading" });
      const result = await commands.listModels(cfg.baseUrl, cfg.apiKey || null);
      if (gen !== fetchGenRef.current) {
        return;
      }
      if (result.status === "ok") {
        const { model, note: newNote } = selectModel(result.data, cfg.model);
        setFetchState({ status: "ok", models: result.data });
        setNote(newNote);
        if (model !== cfg.model) {
          onChange({ ...cfg, model });
        }
      } else {
        setFetchState({
          status: "error",
          error: result.error.message ?? "Could not fetch models",
          models: cfg.model ? [cfg.model] : [],
        });
        setNote(null);
      }
    },
    [onChange],
  );

  // Ref to the latest config so the deferred fetch inside setTimeout
  // always reconciles against the model the user has NOW, not the one
  // captured at effect-run time. Without this, a user model change during
  // the 400ms debounce window gets clobbered by an old snapshot when the
  // fetch response lands.
  const configRef = useRef(config);
  configRef.current = config;

  // Debounced fetch on baseUrl / apiKey change. We intentionally do not
  // react to config.model changes here — a model swap alone shouldn't
  // trigger a new /models probe, and including `config` as a whole would
  // make this effect fire on every parent re-render. The generation is
  // bumped unconditionally so any in-flight fetch is invalidated even
  // while the debounce is still pending or when baseUrl was cleared.
  // The dropdown also enters `loading` state immediately (before the
  // timeout) so the user cannot change `model` during the debounce
  // window and race the pending fetch.
  useEffect(() => {
    clearTimeout(debounceRef.current);
    const gen = ++fetchGenRef.current;
    if (!config.baseUrl.trim()) {
      setFetchState({ status: "idle" });
      return;
    }
    setFetchState({ status: "loading" });
    debounceRef.current = setTimeout(() => {
      fetchModels(configRef.current, gen);
    }, 400);
    return () => clearTimeout(debounceRef.current);
    // Only re-run when connection details change, not on every model change.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [config.baseUrl, config.apiKey, fetchModels]);

  const isLoading = fetchState.status === "loading";
  const availableModels =
    fetchState.status === "ok"
      ? fetchState.models
      : fetchState.status === "error"
        ? fetchState.models
        : config.model
          ? [config.model]
          : [];

  const noUrl = !config.baseUrl.trim();

  return (
    <div>
      <label className="mb-1 block text-xs text-[var(--text-secondary)]">Model</label>
      <div className="flex items-center gap-1.5">
        <select
          value={config.model}
          onChange={(e) => {
            setNote(null);
            onChange({ ...config, model: e.target.value });
          }}
          disabled={noUrl || isLoading}
          className={`${inputClass} flex-1`}
        >
          {noUrl ? (
            <option value="">Enter base URL first</option>
          ) : isLoading ? (
            <option value="">Loading models...</option>
          ) : availableModels.length === 0 ? (
            <option value="">No models found</option>
          ) : (
            availableModels.map((m) => (
              <option key={m} value={m}>
                {m}
              </option>
            ))
          )}
        </select>
        <button
          onClick={() => fetchModels(config, ++fetchGenRef.current)}
          disabled={noUrl || isLoading}
          title="Refresh model list"
          className="shrink-0 rounded bg-[var(--bg-input)] px-2 py-1.5 text-xs text-[var(--text-secondary)] hover:text-[var(--text-primary)] disabled:opacity-40"
        >
          ↻
        </button>
      </div>
      {fetchState.status === "error" && (
        <p className="mt-1 text-[10px] text-red-500" title={fetchState.error}>
          Could not fetch models — showing saved value
        </p>
      )}
      {note && <p className="mt-1 text-[10px] text-yellow-500">{note}</p>}
    </div>
  );
}

function EndpointFields({
  config,
  onChange,
}: {
  config: EndpointConfig;
  onChange: (config: EndpointConfig) => void;
}) {
  return (
    <div className="space-y-2">
      <div>
        <label className="mb-1 block text-xs text-[var(--text-secondary)]">Base URL</label>
        <input
          type="text"
          value={config.baseUrl}
          onChange={(e) => onChange({ ...config, baseUrl: e.target.value })}
          className={inputClass}
        />
      </div>
      <ModelDropdown config={config} onChange={onChange} />
      <div>
        <label className="mb-1 block text-xs text-[var(--text-secondary)]">API Key</label>
        <input
          type="password"
          value={config.apiKey}
          onChange={(e) => onChange({ ...config, apiKey: e.target.value })}
          placeholder="Optional"
          className={`${inputClass} placeholder-[var(--text-muted)]`}
        />
      </div>
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
        <EndpointStatus baseUrl={config.baseUrl} apiKey={config.apiKey} model={config.model} />
      </h3>
      <p className="mb-2 text-[10px] text-[var(--text-muted)]">{description}</p>
      <EndpointFields config={config} onChange={onChange} />
    </div>
  );
}

function ChromeProfileSection() {
  const [profiles, setProfiles] = useState<ChromeProfile[]>([]);
  const [loading, setLoading] = useState(false);
  const [selectedProfileId, setSelectedProfileId] = useState<string | null>(null);
  const [newName, setNewName] = useState("");
  const [showNewInput, setShowNewInput] = useState(false);

  const fetchProfiles = async () => {
    setLoading(true);
    const result = await commands.listChromeProfiles();
    if (result.status === "ok") {
      setProfiles(result.data);
      setSelectedProfileId((prev) => {
        if (!prev && result.data.length > 0) {
          return result.data[0].id;
        }
        return prev;
      });
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
      setSelectedProfileId(result.data.id);
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
          onChange={(e) => setSelectedProfileId(e.target.value)}
          disabled={loading || profiles.length === 0}
          className={inputClass}
        >
          {profiles.map((p) => (
            <option key={p.id} value={p.id}>
              {p.google_email ? `${p.name} (${p.google_email})` : p.name}
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
  supervisorConfig,
  agentConfig,
  fastConfig,
  fastEnabled,
  maxRepairAttempts,
  hoverDwellThreshold,
  supervisionDelayMs,
  toolPermissions,
  traceRetentionDays,
  storeTraces,
  episodicEnabled,
  retrievedEpisodesK,
  episodicGlobalParticipation,
  onClose,
  onSupervisorConfigChange,
  onAgentConfigChange,
  onFastConfigChange,
  onFastEnabledChange,
  onMaxRepairAttemptsChange,
  onHoverDwellThresholdChange,
  onSupervisionDelayMsChange,
  onToolPermissionsChange,
  onToolPermissionChange,
  onTraceRetentionDaysChange,
  onStoreTracesChange,
  onEpisodicEnabledChange,
  onRetrievedEpisodesKChange,
  onEpisodicGlobalParticipationChange,
}: SettingsModalProps) {
  const [tab, setTab] = useState<SettingsTab>("general");

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

        {/* Tab bar */}
        <div className="flex border-b border-[var(--border)]">
          {(["general", "execution", "permissions", "privacy"] as const).map((t) => (
            <button
              key={t}
              onClick={() => setTab(t)}
              className={`px-5 py-2.5 text-xs capitalize ${
                tab === t
                  ? "border-b-2 border-[var(--accent-coral)] font-semibold text-[var(--text-primary)]"
                  : "text-[var(--text-muted)] hover:text-[var(--text-secondary)]"
              }`}
            >
              {t}
            </button>
          ))}
        </div>

        {tab === "permissions" ? (
          <PermissionsTab
            toolPermissions={toolPermissions}
            onToolPermissionsChange={onToolPermissionsChange}
            onToolPermissionChange={onToolPermissionChange}
          />
        ) : tab === "execution" ? (
          <ExecutionTab
            maxRepairAttempts={maxRepairAttempts}
            supervisionDelayMs={supervisionDelayMs}
            episodicEnabled={episodicEnabled}
            retrievedEpisodesK={retrievedEpisodesK}
            episodicGlobalParticipation={episodicGlobalParticipation}
            onMaxRepairAttemptsChange={onMaxRepairAttemptsChange}
            onSupervisionDelayMsChange={onSupervisionDelayMsChange}
            onEpisodicEnabledChange={onEpisodicEnabledChange}
            onRetrievedEpisodesKChange={onRetrievedEpisodesKChange}
            onEpisodicGlobalParticipationChange={
              onEpisodicGlobalParticipationChange
            }
          />
        ) : tab === "privacy" ? (
          <PrivacyTab
            traceRetentionDays={traceRetentionDays}
            storeTraces={storeTraces}
            onTraceRetentionDaysChange={onTraceRetentionDaysChange}
            onStoreTracesChange={onStoreTracesChange}
          />
        ) : (
        <div className="space-y-4 p-4">
          <ConfigSection
            title="Supervisor"
            description="Verdicts saved-workflow steps in Test mode and resolves walkthrough click targets. Typically a larger model."
            config={supervisorConfig}
            onChange={onSupervisorConfigChange}
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
                Fast Model
                {fastEnabled && <EndpointStatus baseUrl={fastConfig.baseUrl} apiKey={fastConfig.apiKey} model={fastConfig.model} />}
              </h3>
              <label className="flex items-center gap-1.5 text-xs text-[var(--text-secondary)] cursor-pointer">
                <input
                  type="checkbox"
                  checked={fastEnabled}
                  onChange={(e) => onFastEnabledChange(e.target.checked)}
                  className="accent-[var(--accent-coral)]"
                />
                Separate model
              </label>
            </div>
            {fastEnabled ? (
              <>
                <p className="mb-2 text-[10px] text-[var(--text-muted)]">
                  Analyzes screenshots and images, returns text summaries to the agent.
                </p>
                <EndpointFields
                  config={fastConfig}
                  onChange={onFastConfigChange}
                />
              </>
            ) : (
              <p className="text-[10px] text-[var(--text-muted)]">
                Using agent model for vision. Enable to use a separate vision model.
              </p>
            )}
          </div>

          <ChromeProfileSection />

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
        )}

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
