import { load } from "@tauri-apps/plugin-store";
import type { EndpointConfig } from "./state";
import { DEFAULT_ENDPOINT, DEFAULT_VLM_ENABLED } from "./state";

export interface PersistedSettings {
  plannerConfig: EndpointConfig;
  agentConfig: EndpointConfig;
  vlmConfig: EndpointConfig;
  vlmEnabled: boolean;
  maxRepairAttempts: number;
  hoverDwellThreshold: number;
  selectedChromeProfileId: string | null;
}

const SETTINGS_DEFAULTS: PersistedSettings = {
  plannerConfig: DEFAULT_ENDPOINT,
  agentConfig: DEFAULT_ENDPOINT,
  vlmConfig: DEFAULT_ENDPOINT,
  vlmEnabled: DEFAULT_VLM_ENABLED,
  maxRepairAttempts: 3,
  hoverDwellThreshold: 2000,
  selectedChromeProfileId: null,
};

export async function loadSettings(): Promise<PersistedSettings> {
  const store = await load("settings.json", { autoSave: false, defaults: {} });

  // Backward compat: if legacy orchestratorConfig exists, use it as fallback for new configs
  const legacyConfig = await store.get<EndpointConfig>("orchestratorConfig");
  const fallback = legacyConfig ?? SETTINGS_DEFAULTS.agentConfig;

  const plannerConfig = await store.get<EndpointConfig>("plannerConfig");
  const agentConfig = await store.get<EndpointConfig>("agentConfig");
  const vlmConfig = await store.get<EndpointConfig>("vlmConfig");
  const vlmEnabled = await store.get<boolean>("vlmEnabled");
  const maxRepairAttempts = await store.get<number>("maxRepairAttempts");
  const hoverDwellThreshold = await store.get<number>("hoverDwellThreshold");
  const selectedChromeProfileId = await store.get<string | null>("selectedChromeProfileId");
  return {
    plannerConfig: plannerConfig ?? fallback,
    agentConfig: agentConfig ?? fallback,
    vlmConfig: vlmConfig ?? SETTINGS_DEFAULTS.vlmConfig,
    vlmEnabled: vlmEnabled ?? SETTINGS_DEFAULTS.vlmEnabled,
    maxRepairAttempts: maxRepairAttempts ?? SETTINGS_DEFAULTS.maxRepairAttempts,
    hoverDwellThreshold: hoverDwellThreshold ?? SETTINGS_DEFAULTS.hoverDwellThreshold,
    selectedChromeProfileId: selectedChromeProfileId ?? SETTINGS_DEFAULTS.selectedChromeProfileId,
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
