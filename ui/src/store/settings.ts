import { load } from "@tauri-apps/plugin-store";
import type { EndpointConfig, ToolPermissions } from "./state";
import { DEFAULT_ENDPOINT, DEFAULT_TOOL_PERMISSIONS, DEFAULT_FAST_ENABLED } from "./state";

export interface PersistedSettings {
  supervisorConfig: EndpointConfig;
  agentConfig: EndpointConfig;
  fastConfig: EndpointConfig;
  fastEnabled: boolean;
  maxRepairAttempts: number;
  hoverDwellThreshold: number;
  supervisionDelayMs: number;
  toolPermissions: ToolPermissions;
}

const SETTINGS_DEFAULTS: PersistedSettings = {
  supervisorConfig: DEFAULT_ENDPOINT,
  agentConfig: DEFAULT_ENDPOINT,
  fastConfig: DEFAULT_ENDPOINT,
  fastEnabled: DEFAULT_FAST_ENABLED,
  maxRepairAttempts: 3,
  hoverDwellThreshold: 2000,
  supervisionDelayMs: 500,
  toolPermissions: DEFAULT_TOOL_PERMISSIONS,
};

export async function loadSettings(): Promise<PersistedSettings> {
  const store = await load("settings.json", { autoSave: false, defaults: {} });

  // Backward compat: if legacy orchestratorConfig exists, use it as fallback for new configs
  const legacyConfig = await store.get<EndpointConfig>("orchestratorConfig");
  const fallback = legacyConfig ?? SETTINGS_DEFAULTS.agentConfig;

  // Migration: plannerConfig → supervisorConfig. The "planner" name was a
  // tombstone from the removed planner pipeline; the config drives the
  // supervisor (step verdict) model. Migrate on next load, then delete the
  // old key so subsequent loads skip the compat path.
  const supervisorConfigStored = await store.get<EndpointConfig>("supervisorConfig");
  const legacyPlannerConfig = await store.get<EndpointConfig>("plannerConfig");
  const supervisorConfig = supervisorConfigStored ?? legacyPlannerConfig ?? fallback;
  if (!supervisorConfigStored && legacyPlannerConfig) {
    await store.set("supervisorConfig", legacyPlannerConfig);
    await store.delete("plannerConfig");
    await store.save();
  }

  const agentConfig = await store.get<EndpointConfig>("agentConfig");
  const maxRepairAttempts = await store.get<number>("maxRepairAttempts");
  const hoverDwellThreshold = await store.get<number>("hoverDwellThreshold");
  const supervisionDelayMs = await store.get<number>("supervisionDelayMs");
  const toolPermissions = await store.get<ToolPermissions>("toolPermissions");

  // Migration: vlmConfig → fastConfig
  const legacyVlmConfig = await store.get<EndpointConfig>("vlmConfig");
  const legacyVlmEnabled = await store.get<boolean>("vlmEnabled");

  const fastConfig =
    (await store.get<EndpointConfig>("fastConfig")) ??
    legacyVlmConfig ??
    SETTINGS_DEFAULTS.fastConfig;

  const fastEnabled =
    (await store.get<boolean>("fastEnabled")) ??
    legacyVlmEnabled ??
    SETTINGS_DEFAULTS.fastEnabled;

  // Clean up old keys if migration happened
  if (legacyVlmConfig && !(await store.get<EndpointConfig>("fastConfig"))) {
    await store.set("fastConfig", fastConfig);
    await store.delete("vlmConfig");
    await store.save();
  }
  if (legacyVlmEnabled !== null && legacyVlmEnabled !== undefined && !(await store.get<boolean>("fastEnabled"))) {
    await store.set("fastEnabled", fastEnabled);
    await store.delete("vlmEnabled");
    await store.save();
  }

  return {
    supervisorConfig,
    agentConfig: agentConfig ?? fallback,
    fastConfig,
    fastEnabled,
    maxRepairAttempts: maxRepairAttempts ?? SETTINGS_DEFAULTS.maxRepairAttempts,
    hoverDwellThreshold: hoverDwellThreshold ?? SETTINGS_DEFAULTS.hoverDwellThreshold,
    supervisionDelayMs: supervisionDelayMs ?? SETTINGS_DEFAULTS.supervisionDelayMs,
    toolPermissions: toolPermissions ?? SETTINGS_DEFAULTS.toolPermissions,
  };
}

export async function saveSetting<K extends keyof PersistedSettings>(
  key: K,
  value: PersistedSettings[K],
): Promise<void> {
  const store = await load("settings.json", { autoSave: false, defaults: {} });
  await store.set(key, value);
  await store.save();
}

export function toEndpoint(c: EndpointConfig) {
  return {
    base_url: c.baseUrl,
    model: c.model,
    api_key: c.apiKey || null,
  };
}
