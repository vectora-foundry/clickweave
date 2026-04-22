import { describe, it, expect } from "vitest";
import { selectModel } from "./SettingsModal";

describe("selectModel", () => {
  it("keeps current model when it is in the list", () => {
    const { model, note } = selectModel(["ModelA", "ModelB"], "ModelA");
    expect(model).toBe("ModelA");
    expect(note).toBeNull();
  });

  it("selects the only model when list has one entry and current is absent", () => {
    const { model, note } = selectModel(["OnlyModel"], "stale-model");
    expect(model).toBe("OnlyModel");
    expect(note).toBeNull();
  });

  it("selects first model and surfaces a note when list has multiple and current is absent", () => {
    const { model, note } = selectModel(["FirstModel", "SecondModel"], "stale-model");
    expect(model).toBe("FirstModel");
    expect(note).toContain("FirstModel");
  });

  it("returns current model unchanged when list is empty", () => {
    const { model, note } = selectModel([], "saved-model");
    expect(model).toBe("saved-model");
    expect(note).toBeNull();
  });

  it("canonicalizes to the server id when fuzzy match strips .gguf suffix", () => {
    // User has Qwen3-27B.gguf stored; server advertises Qwen3-27B. The
    // fuzzy match must return the server's id so the <select> has a
    // matching <option>, instead of leaving the control blank.
    const { model, note } = selectModel(
      ["Qwen3-27B", "Qwen3-14B"],
      "Qwen3-27B.gguf",
    );
    expect(model).toBe("Qwen3-27B");
    expect(note).toBeNull();
  });

  it("canonicalizes to the server id when server returns a path-prefixed id", () => {
    const { model, note } = selectModel(
      ["/models/Qwen3-27B.gguf"],
      "Qwen3-27B.gguf",
    );
    expect(model).toBe("/models/Qwen3-27B.gguf");
    expect(note).toBeNull();
  });

  it("canonicalizes config path-prefixed id to server bare id", () => {
    const { model, note } = selectModel(
      ["Qwen3-27B.gguf"],
      "/models/Qwen3-27B.gguf",
    );
    expect(model).toBe("Qwen3-27B.gguf");
    expect(note).toBeNull();
  });

  it("prefers exact match over fuzzy match", () => {
    const { model, note } = selectModel(
      ["Qwen3-27B.gguf", "Qwen3-27B", "/models/Qwen3-27B"],
      "Qwen3-27B.gguf",
    );
    expect(model).toBe("Qwen3-27B.gguf");
    expect(note).toBeNull();
  });
});
