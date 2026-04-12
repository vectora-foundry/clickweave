import { commands } from "../bindings";
import type { EndpointConfig } from "./state";
import type { PersistedSettings } from "./settings";

/// Label used in log messages so the user can tell which configured model
/// is reporting a problem.
export type ModelRole = "agent" | "supervisor" | "fast";

export interface ModelCheck {
    role: ModelRole;
    config: EndpointConfig;
}

export type ModelAvailabilityStatus =
    | { role: ModelRole; config: EndpointConfig; available: true }
    | { role: ModelRole; config: EndpointConfig; available: false; error: string };

/// Pure helper: given the persisted settings, return the list of endpoints
/// that should be probed on boot. Endpoints with an empty base URL or model
/// are skipped (they cannot be checked). The fast endpoint is skipped when
/// `fastEnabled` is false because it is not used by the executor.
export function buildModelChecks(settings: PersistedSettings): ModelCheck[] {
    const checks: ModelCheck[] = [];
    const push = (role: ModelRole, config: EndpointConfig) => {
        if (config.baseUrl.trim() !== "" && config.model.trim() !== "") {
            checks.push({ role, config });
        }
    };
    push("agent", settings.agentConfig);
    push("supervisor", settings.supervisorConfig);
    if (settings.fastEnabled) {
        push("fast", settings.fastConfig);
    }
    return checks;
}

/// Format a status entry into a user-facing log line. Kept pure so it can be
/// unit tested without hitting the backend.
export function formatModelStatus(status: ModelAvailabilityStatus): string {
    const { role, config } = status;
    if (status.available) {
        return `Model check: ${role} (${config.model} @ ${config.baseUrl}) is available`;
    }
    return `Model check: ${role} (${config.model} @ ${config.baseUrl}) is unavailable — ${status.error}`;
}

/// Probe a single endpoint via the existing Tauri command and normalise the
/// result into a `ModelAvailabilityStatus`.
async function probe(check: ModelCheck): Promise<ModelAvailabilityStatus> {
    const { role, config } = check;
    const apiKey = config.apiKey.trim() === "" ? null : config.apiKey;
    const model = config.model.trim() === "" ? null : config.model;
    const result = await commands.checkEndpoint(config.baseUrl, apiKey, model);
    if (result.status === "ok") {
        return { role, config, available: true };
    }
    const error = result.error.message ?? "Unreachable";
    return { role, config, available: false, error };
}

/// Probe all configured endpoints in parallel. Failures surface as
/// `available: false` entries rather than throwing, so a single bad endpoint
/// does not mask the status of the others.
export async function verifyConfiguredModels(
    settings: PersistedSettings,
): Promise<ModelAvailabilityStatus[]> {
    const checks = buildModelChecks(settings);
    return Promise.all(checks.map(probe));
}
