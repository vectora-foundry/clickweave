import { describe, it, expect, vi } from "vitest";

vi.mock("../bindings", () => ({
    commands: {
        checkEndpoint: vi.fn(),
    },
}));

import { commands } from "../bindings";
import {
    buildModelChecks,
    formatModelStatus,
    verifyConfiguredModels,
} from "./modelAvailability";
import type { PersistedSettings } from "./settings";
import type { EndpointConfig } from "./state";
import { DEFAULT_ENDPOINT, DEFAULT_TOOL_PERMISSIONS } from "./state";

function makeSettings(overrides: Partial<PersistedSettings> = {}): PersistedSettings {
    return {
        supervisorConfig: DEFAULT_ENDPOINT,
        agentConfig: DEFAULT_ENDPOINT,
        fastConfig: DEFAULT_ENDPOINT,
        fastEnabled: false,
        maxRepairAttempts: 3,
        hoverDwellThreshold: 2000,
        supervisionDelayMs: 500,
        toolPermissions: DEFAULT_TOOL_PERMISSIONS,
        ...overrides,
    };
}

describe("buildModelChecks", () => {
    it("includes agent and supervisor by default, excludes fast when disabled", () => {
        const checks = buildModelChecks(makeSettings());
        const roles = checks.map((c) => c.role);
        expect(roles).toEqual(["agent", "supervisor"]);
    });

    it("includes fast when fastEnabled is true", () => {
        const checks = buildModelChecks(makeSettings({ fastEnabled: true }));
        const roles = checks.map((c) => c.role);
        expect(roles).toEqual(["agent", "supervisor", "fast"]);
    });

    it("skips endpoints with empty base URL", () => {
        const empty: EndpointConfig = { baseUrl: "", apiKey: "", model: "local" };
        const checks = buildModelChecks(makeSettings({ agentConfig: empty }));
        expect(checks.map((c) => c.role)).toEqual(["supervisor"]);
    });

    it("skips endpoints with empty model", () => {
        const empty: EndpointConfig = { baseUrl: "http://x", apiKey: "", model: "" };
        const checks = buildModelChecks(makeSettings({ supervisorConfig: empty }));
        expect(checks.map((c) => c.role)).toEqual(["agent"]);
    });

    it("skips fast endpoint when enabled but blank", () => {
        const empty: EndpointConfig = { baseUrl: "", apiKey: "", model: "" };
        const checks = buildModelChecks(
            makeSettings({ fastEnabled: true, fastConfig: empty }),
        );
        expect(checks.map((c) => c.role)).toEqual(["agent", "supervisor"]);
    });
});

describe("formatModelStatus", () => {
    const config: EndpointConfig = {
        baseUrl: "http://localhost:1234/v1",
        apiKey: "",
        model: "local",
    };

    it("reports available endpoints with role, model, and URL", () => {
        const msg = formatModelStatus({ role: "agent", config, available: true });
        expect(msg).toContain("agent");
        expect(msg).toContain("local");
        expect(msg).toContain("http://localhost:1234/v1");
        expect(msg).toContain("available");
    });

    it("includes the error text for unavailable endpoints", () => {
        const msg = formatModelStatus({
            role: "supervisor",
            config,
            available: false,
            error: "connection refused",
        });
        expect(msg).toContain("supervisor");
        expect(msg).toContain("unavailable");
        expect(msg).toContain("connection refused");
    });
});

describe("verifyConfiguredModels", () => {
    it("records a thrown invoke as unavailable without failing the batch", async () => {
        const settings = makeSettings({
            agentConfig: { baseUrl: "http://agent", apiKey: "", model: "m-a" },
            supervisorConfig: { baseUrl: "http://supervisor", apiKey: "", model: "m-s" },
        });
        const mock = vi.mocked(commands.checkEndpoint);
        mock.mockReset();
        mock.mockImplementationOnce(async () => {
            throw new Error("IPC transport exploded");
        });
        mock.mockImplementationOnce(async () => ({ status: "ok", data: null }));

        const results = await verifyConfiguredModels(settings);

        expect(results).toHaveLength(2);
        expect(results[0]).toMatchObject({
            role: "agent",
            available: false,
            error: "IPC transport exploded",
        });
        expect(results[1]).toMatchObject({ role: "supervisor", available: true });
    });
});
