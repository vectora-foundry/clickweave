import { describe, it, expect } from "vitest";
import { extractOutputRefs, generateAutoId, nodeTypeName } from "./outputSchema";

describe("nodeTypeName", () => {
  it("returns the type field from an internally-tagged enum", () => {
    expect(nodeTypeName({ type: "FindText", text: "hello" })).toBe("FindText");
  });

  it("returns empty string when type is missing", () => {
    expect(nodeTypeName({})).toBe("");
  });
});

describe("extractOutputRefs", () => {
  it("extracts _ref fields from internally-tagged nodeType", () => {
    const nodeType = { type: "Click", target: null, target_ref: { node: "find_text_1", field: "coordinates" }, button: "Left" };
    const refs = extractOutputRefs(nodeType);
    expect(refs).toHaveLength(1);
    expect(refs[0]!.key).toBe("target_ref");
    expect(refs[0]!.ref).toEqual({ node: "find_text_1", field: "coordinates" });
  });

  it("skips null _ref fields", () => {
    const nodeType = { type: "Click", target_ref: null, button: "Left" };
    const refs = extractOutputRefs(nodeType);
    expect(refs).toHaveLength(0);
  });

  it("returns empty for nodes with no _ref fields", () => {
    const nodeType = { type: "PressKey", key: "Enter" };
    expect(extractOutputRefs(nodeType)).toHaveLength(0);
  });
});

describe("generateAutoId", () => {
  it("generates first id when counters are empty", () => {
    const counters: Record<string, number> = {};
    const result = generateAutoId("FindText", counters);
    expect(result.autoId).toBe("find_text_1");
    expect(result.counter).toBe(1);
  });

  it("increments counter from existing value", () => {
    const counters: Record<string, number> = { find_text: 2 };
    const result = generateAutoId("FindText", counters);
    expect(result.autoId).toBe("find_text_3");
    expect(result.counter).toBe(3);
  });

  it("does not reuse deleted IDs (monotonic)", () => {
    // Counter was at 5, node was deleted, but counter stays at 5
    const counters: Record<string, number> = { click: 5 };
    const result = generateAutoId("Click", counters);
    expect(result.autoId).toBe("click_6");
  });

  it("handles different types independently", () => {
    const counters: Record<string, number> = { find_text: 2, click: 1 };
    expect(generateAutoId("FindText", counters).autoId).toBe("find_text_3");
    expect(generateAutoId("Click", counters).autoId).toBe("click_2");
    expect(generateAutoId("Hover", counters).autoId).toBe("hover_1");
  });

  it("returns the base for counter map updates", () => {
    const counters: Record<string, number> = {};
    const result = generateAutoId("AiStep", counters);
    expect(result.base).toBe("ai_step");
    expect(result.counter).toBe(1);
  });
});
