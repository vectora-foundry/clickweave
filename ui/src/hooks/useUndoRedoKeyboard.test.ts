import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { renderHook } from "@testing-library/react";
import { useUndoRedoKeyboard, isTextInput } from "./useUndoRedoKeyboard";

function fireKey(key: string, modifiers: { metaKey?: boolean; ctrlKey?: boolean; shiftKey?: boolean } = {}) {
  const event = new KeyboardEvent("keydown", {
    key,
    metaKey: modifiers.metaKey ?? false,
    ctrlKey: modifiers.ctrlKey ?? false,
    shiftKey: modifiers.shiftKey ?? false,
    bubbles: true,
    cancelable: true,
  });
  window.dispatchEvent(event);
  return event;
}

describe("useUndoRedoKeyboard", () => {
  let undo: ReturnType<typeof vi.fn<() => void>>;
  let redo: ReturnType<typeof vi.fn<() => void>>;

  beforeEach(() => {
    undo = vi.fn();
    redo = vi.fn();
    renderHook(() => useUndoRedoKeyboard(undo, redo));
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("calls undo on Cmd+Z (macOS)", () => {
    fireKey("z", { metaKey: true });
    expect(undo).toHaveBeenCalledOnce();
    expect(redo).not.toHaveBeenCalled();
  });

  it("calls redo on Cmd+Shift+Z (macOS)", () => {
    fireKey("Z", { metaKey: true, shiftKey: true });
    expect(redo).toHaveBeenCalledOnce();
    expect(undo).not.toHaveBeenCalled();
  });

  it("does nothing without modifier keys", () => {
    fireKey("z");
    expect(undo).not.toHaveBeenCalled();
    expect(redo).not.toHaveBeenCalled();
  });

  it("does nothing for non-z keys with modifier", () => {
    fireKey("a", { metaKey: true });
    expect(undo).not.toHaveBeenCalled();
    expect(redo).not.toHaveBeenCalled();
  });

  it("does nothing when an input element is focused", () => {
    const input = document.createElement("input");
    document.body.appendChild(input);
    input.focus();

    fireKey("z", { metaKey: true });
    expect(undo).not.toHaveBeenCalled();

    document.body.removeChild(input);
  });

});

describe("isTextInput", () => {
  it("returns false for null", () => {
    expect(isTextInput(null)).toBe(false);
  });

  it("returns true for INPUT elements", () => {
    expect(isTextInput(document.createElement("input"))).toBe(true);
  });

  it("returns true for TEXTAREA elements", () => {
    expect(isTextInput(document.createElement("textarea"))).toBe(true);
  });

  it("returns true for contentEditable elements", () => {
    // jsdom doesn't implement isContentEditable, so we mock it
    const div = document.createElement("div");
    Object.defineProperty(div, "isContentEditable", { value: true });
    expect(isTextInput(div)).toBe(true);
  });

  it("returns false for regular elements", () => {
    expect(isTextInput(document.createElement("div"))).toBe(false);
    expect(isTextInput(document.createElement("button"))).toBe(false);
  });
});
