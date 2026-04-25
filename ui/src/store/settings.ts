import { load } from "@tauri-apps/plugin-store";
import type { EndpointConfig, ToolPermissions } from "./state";
import {
  DEFAULT_ENDPOINT,
  DEFAULT_FAST_ENABLED,
  DEFAULT_TOOL_PERMISSIONS,
} from "./state";

/**
 * Fill in missing fields from an older 2-tier permissions blob. Older
 * versions only had `allowAll` + `tools`; the new fields default to
 * the project defaults so an upgrade is silent.
 */
export function normalizeToolPermissions(
  raw: Partial<ToolPermissions>,
): ToolPermissions {
  return {
    allowAll: raw.allowAll ?? DEFAULT_TOOL_PERMISSIONS.allowAll,
    tools: raw.tools ?? {},
    patternRules: raw.patternRules ?? [],
    requireConfirmDestructive:
      raw.requireConfirmDestructive ??
      DEFAULT_TOOL_PERMISSIONS.requireConfirmDestructive,
    consecutiveDestructiveCap:
      raw.consecutiveDestructiveCap ??
      DEFAULT_TOOL_PERMISSIONS.consecutiveDestructiveCap,
  };
}

export interface PersistedSettings {
  supervisorConfig: EndpointConfig;
  agentConfig: EndpointConfig;
  fastConfig: EndpointConfig;
  fastEnabled: boolean;
  maxRepairAttempts: number;
  hoverDwellThreshold: number;
  supervisionDelayMs: number;
  toolPermissions: ToolPermissions;
  /** Privacy: retention window for run traces. 0 = never delete. */
  traceRetentionDays: number;
  /** Privacy: global kill switch for on-disk run traces. */
  storeTraces: boolean;
  /** Spec 2 master kill switch for episodic memory. */
  episodicEnabled: boolean;
  /** Spec 2 retrieval depth — top-k episodes to retrieve per trigger. */
  retrievedEpisodesK: number;
  /** Spec 2 D35 privacy opt-in for the global cross-workflow store. */
  episodicGlobalParticipation: boolean;
}

export const DEFAULT_TRACE_RETENTION_DAYS = 30;
export const DEFAULT_STORE_TRACES = true;
export const DEFAULT_EPISODIC_ENABLED = true;
export const DEFAULT_RETRIEVED_EPISODES_K = 2;
export const DEFAULT_EPISODIC_GLOBAL_PARTICIPATION = false;

const SETTINGS_DEFAULTS: PersistedSettings = {
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
  // Pull the raw blob first so an older 2-tier shape still loads — we
  // fill in the new fields with defaults for back-compat.
  const rawToolPermissions =
    await store.get<Partial<ToolPermissions>>("toolPermissions");
  const toolPermissions = rawToolPermissions
    ? normalizeToolPermissions(rawToolPermissions)
    : undefined;

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

  const traceRetentionDays = await store.get<number>("traceRetentionDays");
  const storeTraces = await store.get<boolean>("storeTraces");
  const episodicEnabled = await store.get<boolean>("episodicEnabled");
  const retrievedEpisodesK = await store.get<number>("retrievedEpisodesK");
  const episodicGlobalParticipation = await store.get<boolean>(
    "episodicGlobalParticipation",
  );

  return {
    supervisorConfig,
    agentConfig: agentConfig ?? fallback,
    fastConfig,
    fastEnabled,
    maxRepairAttempts: maxRepairAttempts ?? SETTINGS_DEFAULTS.maxRepairAttempts,
    hoverDwellThreshold: hoverDwellThreshold ?? SETTINGS_DEFAULTS.hoverDwellThreshold,
    supervisionDelayMs: supervisionDelayMs ?? SETTINGS_DEFAULTS.supervisionDelayMs,
    toolPermissions: toolPermissions ?? SETTINGS_DEFAULTS.toolPermissions,
    traceRetentionDays: traceRetentionDays ?? SETTINGS_DEFAULTS.traceRetentionDays,
    storeTraces: storeTraces ?? SETTINGS_DEFAULTS.storeTraces,
    episodicEnabled: episodicEnabled ?? SETTINGS_DEFAULTS.episodicEnabled,
    retrievedEpisodesK:
      retrievedEpisodesK ?? SETTINGS_DEFAULTS.retrievedEpisodesK,
    episodicGlobalParticipation:
      episodicGlobalParticipation ??
      SETTINGS_DEFAULTS.episodicGlobalParticipation,
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
