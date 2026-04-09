import type { StateCreator } from "zustand";
import type { EndpointConfig, ToolPermissions } from "../state";
import { DEFAULT_ENDPOINT, DEFAULT_TOOL_PERMISSIONS, DEFAULT_FAST_ENABLED } from "../state";
import { loadSettings, saveSetting } from "../settings";
import type { PersistedSettings } from "../settings";
import type { StoreState } from "./types";

export interface SettingsSlice {
  plannerConfig: EndpointConfig;
  agentConfig: EndpointConfig;
  fastConfig: EndpointConfig;
  fastEnabled: boolean;
  maxRepairAttempts: number;
  hoverDwellThreshold: number;
  outcomeDelayMs: number;
  supervisionDelayMs: number;
  toolPermissions: ToolPermissions;
  _settingsLoaded: boolean;

  loadSettingsFromDisk: () => void;
  setPlannerConfig: (config: EndpointConfig) => void;
  setAgentConfig: (config: EndpointConfig) => void;
  setFastConfig: (config: EndpointConfig) => void;
  setFastEnabled: (enabled: boolean) => void;
  setMaxRepairAttempts: (n: number) => void;
  setHoverDwellThreshold: (ms: number) => void;
  setOutcomeDelayMs: (ms: number) => void;
  setSupervisionDelayMs: (ms: number) => void;
  setToolPermissions: (perms: ToolPermissions) => void;
  setToolPermission: (toolName: string, level: "ask" | "allow") => Promise<void>;
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
  plannerConfig: DEFAULT_ENDPOINT,
  agentConfig: DEFAULT_ENDPOINT,
  fastConfig: DEFAULT_ENDPOINT,
  fastEnabled: DEFAULT_FAST_ENABLED,
  maxRepairAttempts: 3,
  hoverDwellThreshold: 2000,
  outcomeDelayMs: 1000,
  supervisionDelayMs: 500,
  toolPermissions: DEFAULT_TOOL_PERMISSIONS,
  _settingsLoaded: false,

  loadSettingsFromDisk: () => {
    if (get()._settingsLoaded) return;
    set({ _settingsLoaded: true });
    loadSettings()
      .then((s) => {
        set({
          plannerConfig: s.plannerConfig,
          agentConfig: s.agentConfig,
          fastConfig: s.fastConfig,
          fastEnabled: s.fastEnabled,
          maxRepairAttempts: clampInt(s.maxRepairAttempts, 0, 10, 3),
          hoverDwellThreshold: clampInt(s.hoverDwellThreshold, 100, 10000, 2000),
          outcomeDelayMs: clampInt(s.outcomeDelayMs, 0, 10000, 1000),
          supervisionDelayMs: clampInt(s.supervisionDelayMs, 0, 10000, 500),
          toolPermissions: s.toolPermissions,
        });
      })
      .catch((e) => console.error("Failed to load settings:", e));
  },

  setPlannerConfig: (config) => persistSetting("plannerConfig", config, set),
  setAgentConfig: (config) => persistSetting("agentConfig", config, set),
  setFastConfig: (config) => persistSetting("fastConfig", config, set),
  setFastEnabled: (enabled) => persistSetting("fastEnabled", enabled, set),
  setMaxRepairAttempts: (n) => persistSetting("maxRepairAttempts", clampInt(n, 0, 10, 3), set),
  setHoverDwellThreshold: (ms) => persistSetting("hoverDwellThreshold", clampInt(ms, 100, 10000, 2000), set),
  setOutcomeDelayMs: (ms) => persistSetting("outcomeDelayMs", clampInt(ms, 0, 10000, 1000), set),
  setSupervisionDelayMs: (ms) => persistSetting("supervisionDelayMs", clampInt(ms, 0, 10000, 500), set),
  setToolPermissions: (perms) => persistSetting("toolPermissions", perms, set),
  setToolPermission: (toolName, level) => {
    const current = get().toolPermissions;
    const updated = { ...current, tools: { ...current.tools, [toolName]: level } };
    return persistSetting("toolPermissions", updated, set);
  },
});
