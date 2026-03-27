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
  it("generates first id when no existing nodes", () => {
    expect(generateAutoId("FindText", [])).toBe("find_text_1");
  });

  it("increments counter based on existing auto_ids", () => {
    expect(generateAutoId("FindText", ["find_text_1", "find_text_2"])).toBe("find_text_3");
  });

  it("handles gaps in numbering", () => {
    expect(generateAutoId("Click", ["click_1", "click_5"])).toBe("click_6");
  });

  it("ignores unrelated auto_ids", () => {
    expect(generateAutoId("Click", ["find_text_1", "hover_3"])).toBe("click_1");
  });

  it("handles undefined entries in existing ids", () => {
    expect(generateAutoId("AiStep", [undefined, "ai_step_1", undefined])).toBe("ai_step_2");
  });

  it("handles mixed node types correctly", () => {
    const existing = ["find_text_1", "click_1", "find_text_2", "click_2"];
    expect(generateAutoId("FindText", existing)).toBe("find_text_3");
    expect(generateAutoId("Click", existing)).toBe("click_3");
    expect(generateAutoId("Hover", existing)).toBe("hover_1");
  });
});
