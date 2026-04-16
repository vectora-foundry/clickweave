import { describe, it, expect } from "vitest";
import {
  DEFAULT_STORE_TRACES,
  DEFAULT_TRACE_RETENTION_DAYS,
  normalizeToolPermissions,
  toEndpoint,
} from "./settings";

describe("toEndpoint", () => {
  it("maps camelCase UI config to snake_case backend config", () => {
    const result = toEndpoint({
      baseUrl: "http://localhost:1234/v1",
      apiKey: "sk-test",
      model: "gpt-4",
    });
    expect(result).toEqual({
      base_url: "http://localhost:1234/v1",
      api_key: "sk-test",
      model: "gpt-4",
    });
  });

  it("converts empty apiKey to null", () => {
    const result = toEndpoint({
      baseUrl: "http://localhost:1234/v1",
      apiKey: "",
      model: "local",
    });
    expect(result.api_key).toBeNull();
  });
});

describe("normalizeToolPermissions", () => {
  it("fills in new fields when loading a legacy 2-tier blob", () => {
    // Older blob shape: only allowAll + tools exist.
    const legacy = {
      allowAll: false,
      tools: { click: "allow" as const },
    };
    const result = normalizeToolPermissions(legacy);
    expect(result).toEqual({
      allowAll: false,
      tools: { click: "allow" },
      patternRules: [],
      requireConfirmDestructive: true,
      consecutiveDestructiveCap: 3,
    });
  });

  it("preserves explicit values when all fields are present", () => {
    const stored = {
      allowAll: true,
      tools: { click: "deny" as const },
      patternRules: [
        { toolPattern: "cdp_*", action: "allow" as const },
      ],
      requireConfirmDestructive: false,
      consecutiveDestructiveCap: 0,
    };
    const result = normalizeToolPermissions(stored);
    expect(result).toEqual(stored);
  });
});

describe("privacy defaults", () => {
  it("keeps trace retention at 30 days by default", () => {
    expect(DEFAULT_TRACE_RETENTION_DAYS).toBe(30);
  });

  it("keeps the store-traces kill switch on by default", () => {
    expect(DEFAULT_STORE_TRACES).toBe(true);
  });
});
