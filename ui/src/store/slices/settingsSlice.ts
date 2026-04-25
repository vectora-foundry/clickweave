import type { StateCreator } from "zustand";
import type { EndpointConfig, PermissionLevel, ToolPermissions } from "../state";
import { DEFAULT_ENDPOINT, DEFAULT_TOOL_PERMISSIONS, DEFAULT_FAST_ENABLED } from "../state";
import { formatModelStatus, verifyConfiguredModels } from "../modelAvailability";
import {
  DEFAULT_EPISODIC_ENABLED,
  DEFAULT_EPISODIC_GLOBAL_PARTICIPATION,
  DEFAULT_RETRIEVED_EPISODES_K,
  DEFAULT_STORE_TRACES,
  DEFAULT_TRACE_RETENTION_DAYS,
  loadSettings,
  saveSetting,
} from "../settings";
import type { PersistedSettings } from "../settings";
import type { StoreState } from "./types";

export interface SettingsSlice {
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
  _settingsLoaded: boolean;

  loadSettingsFromDisk: () => void;
  setSupervisorConfig: (config: EndpointConfig) => void;
  setAgentConfig: (config: EndpointConfig) => void;
  setFastConfig: (config: EndpointConfig) => void;
  setFastEnabled: (enabled: boolean) => void;
  setMaxRepairAttempts: (n: number) => void;
  setHoverDwellThreshold: (ms: number) => void;
  setSupervisionDelayMs: (ms: number) => void;
  setToolPermissions: (perms: ToolPermissions) => void;
  setToolPermission: (toolName: string, level: PermissionLevel) => Promise<void>;
  setTraceRetentionDays: (days: number) => void;
  setStoreTraces: (enabled: boolean) => void;
  setEpisodicEnabled: (enabled: boolean) => void;
  setRetrievedEpisodesK: (n: number) => void;
  setEpisodicGlobalParticipation: (enabled: boolean) => void;
}

function persistSetting<K extends keyof PersistedSettings>(
  key: K,
  value: PersistedSettings[K],
  set: (partial: Partial<StoreState>) => void,
): Promise<void> {
  set({ [key]: value } as Partial<StoreState>);
  return saveSetting(key, value).catch((e) => {
    console.error(`Failed to save setting "${key}":`, e);
  });
}

function clampInt(value: unknown, min: number, max: number, fallback: number): number {
  const n = Number(value);
  if (!Number.isFinite(n)) return fallback;
  return Math.max(min, Math.min(max, Math.floor(n)));
}

export const createSettingsSlice: StateCreator<StoreState, [], [], SettingsSlice> = (set, get) => ({
  supervisorConfig: DEFAULT_ENDPOINT,
  agentConfig: DEFAULT_ENDPOINT,
  fastConfig: DEFAULT_ENDPOINT,
  fastEnabled: DEFAULT_FAST_ENABLED,
  maxRepairAttempts: 3,
  hoverDwellThreshold: 2000,
  supervisionDelayMs: 500,
  toolPermissions: DEFAULT_TOOL_PERMISSIONS,
  traceRetentionDays: DEFAULT_TRACE_RETENTION_DAYS,
  storeTraces: DEFAULT_STORE_TRACES,
  episodicEnabled: DEFAULT_EPISODIC_ENABLED,
  retrievedEpisodesK: DEFAULT_RETRIEVED_EPISODES_K,
  episodicGlobalParticipation: DEFAULT_EPISODIC_GLOBAL_PARTICIPATION,
  _settingsLoaded: false,

  loadSettingsFromDisk: () => {
    if (get()._settingsLoaded) return;
    set({ _settingsLoaded: true });
    loadSettings()
      .then((s) => {
        set({
          supervisorConfig: s.supervisorConfig,
          agentConfig: s.agentConfig,
          fastConfig: s.fastConfig,
          fastEnabled: s.fastEnabled,
          maxRepairAttempts: clampInt(s.maxRepairAttempts, 0, 10, 3),
          hoverDwellThreshold: clampInt(s.hoverDwellThreshold, 100, 10000, 2000),
          supervisionDelayMs: clampInt(s.supervisionDelayMs, 0, 10000, 500),
          toolPermissions: s.toolPermissions,
          traceRetentionDays: clampInt(
            s.traceRetentionDays,
            0,
            3650,
            DEFAULT_TRACE_RETENTION_DAYS,
          ),
          storeTraces: s.storeTraces,
          episodicEnabled: s.episodicEnabled,
          retrievedEpisodesK: clampInt(
            s.retrievedEpisodesK,
            1,
            10,
            DEFAULT_RETRIEVED_EPISODES_K,
          ),
          episodicGlobalParticipation: s.episodicGlobalParticipation,
        });
        verifyConfiguredModels(s)
          .then((results) => {
            const pushLog = get().pushLog;
            for (const status of results) {
              pushLog(formatModelStatus(status));
            }
          })
          .catch((e) => console.error("Model availability check failed:", e));
      })
      .catch((e) => console.error("Failed to load settings:", e));
  },

  setSupervisorConfig: (config) => persistSetting("supervisorConfig", config, set),
  setAgentConfig: (config) => persistSetting("agentConfig", config, set),
  setFastConfig: (config) => persistSetting("fastConfig", config, set),
  setFastEnabled: (enabled) => persistSetting("fastEnabled", enabled, set),
  setMaxRepairAttempts: (n) => persistSetting("maxRepairAttempts", clampInt(n, 0, 10, 3), set),
  setHoverDwellThreshold: (ms) => persistSetting("hoverDwellThreshold", clampInt(ms, 100, 10000, 2000), set),
  setSupervisionDelayMs: (ms) => persistSetting("supervisionDelayMs", clampInt(ms, 0, 10000, 500), set),
  setToolPermissions: (perms) => persistSetting("toolPermissions", perms, set),
  setToolPermission: (toolName, level) => {
    const current = get().toolPermissions;
    const updated = { ...current, tools: { ...current.tools, [toolName]: level } };
    return persistSetting("toolPermissions", updated, set);
  },
  setTraceRetentionDays: (days) =>
    persistSetting(
      "traceRetentionDays",
      clampInt(days, 0, 3650, DEFAULT_TRACE_RETENTION_DAYS),
      set,
    ),
  setStoreTraces: (enabled) => persistSetting("storeTraces", enabled, set),
  setEpisodicEnabled: (enabled) =>
    persistSetting("episodicEnabled", enabled, set),
  setRetrievedEpisodesK: (n) =>
    persistSetting(
      "retrievedEpisodesK",
      clampInt(n, 1, 10, DEFAULT_RETRIEVED_EPISODES_K),
      set,
    ),
  setEpisodicGlobalParticipation: (enabled) =>
    persistSetting("episodicGlobalParticipation", enabled, set),
});
