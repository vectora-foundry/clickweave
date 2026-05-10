import { describe, it, expect } from "vitest";
import { DEFAULT_ENDPOINT, PROJECT_SCHEMA_VERSION } from "./state";

describe("DEFAULT_ENDPOINT", () => {
  it("has localhost base URL", () => {
    expect(DEFAULT_ENDPOINT.baseUrl).toContain("localhost");
  });

  it("has empty apiKey", () => {
    expect(DEFAULT_ENDPOINT.apiKey).toBe("");
  });
});

describe("PROJECT_SCHEMA_VERSION", () => {
  it("is a positive integer", () => {
    expect(PROJECT_SCHEMA_VERSION).toBeGreaterThan(0);
    expect(Number.isInteger(PROJECT_SCHEMA_VERSION)).toBe(true);
  });
});
