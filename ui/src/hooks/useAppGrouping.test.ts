import { describe, it, expect } from "vitest";
import { renderHook, act } from "@testing-library/react";
import { useAppGrouping } from "./useAppGrouping";
import { node, edge, makeWorkflow } from "./test-helpers";

describe("useAppGrouping", () => {
  it("computes appGroups from workflow", () => {
    const wf = makeWorkflow(
      [
        node("fw1", "FocusWindow", { method: "AppName", value: "Discord", bring_to_front: true }),
        node("c1", "Click"),
      ],
      [edge("fw1", "c1")],
    );
    const { result } = renderHook(() => useAppGrouping(wf));
    expect(result.current.appGroups.has("appgroup-fw1")).toBe(true);
    expect(result.current.appGroups.get("appgroup-fw1")).toEqual(["fw1", "c1"]);
  });

  it("computes nodeToAppGroup inverse map", () => {
    const wf = makeWorkflow(
      [
        node("fw1", "FocusWindow", { method: "AppName", value: "Discord", bring_to_front: true }),
        node("c1", "Click"),
      ],
      [edge("fw1", "c1")],
    );
    const { result } = renderHook(() => useAppGrouping(wf));
    expect(result.current.nodeToAppGroup.get("fw1")).toBe("appgroup-fw1");
    expect(result.current.nodeToAppGroup.get("c1")).toBe("appgroup-fw1");
  });

  it("computes appGroupMeta with name, color, and anchorId", () => {
    const wf = makeWorkflow(
      [
        node("fw1", "FocusWindow", { method: "AppName", value: "Discord", bring_to_front: true }),
        node("c1", "Click"),
      ],
      [edge("fw1", "c1")],
    );
    const { result } = renderHook(() => useAppGrouping(wf));
    const meta = result.current.appGroupMeta.get("appgroup-fw1");
    expect(meta?.appName).toBe("Discord");
    expect(meta?.anchorId).toBe("fw1");
    expect(typeof meta?.color).toBe("string");
  });

  it("new groups default to collapsed", () => {
    const wf = makeWorkflow(
      [
        node("fw1", "FocusWindow", { method: "AppName", value: "Discord", bring_to_front: true }),
        node("c1", "Click"),
      ],
      [edge("fw1", "c1")],
    );
    const { result } = renderHook(() => useAppGrouping(wf));
    expect(result.current.collapsedApps.has("appgroup-fw1")).toBe(true);
  });

  it("toggleAppCollapse toggles collapsed state", () => {
    const wf = makeWorkflow(
      [
        node("fw1", "FocusWindow", { method: "AppName", value: "Discord", bring_to_front: true }),
        node("c1", "Click"),
      ],
      [edge("fw1", "c1")],
    );
    const { result } = renderHook(() => useAppGrouping(wf));
    expect(result.current.collapsedApps.has("appgroup-fw1")).toBe(true);

    act(() => result.current.toggleAppCollapse("appgroup-fw1"));
    expect(result.current.collapsedApps.has("appgroup-fw1")).toBe(false);

    act(() => result.current.toggleAppCollapse("appgroup-fw1"));
    expect(result.current.collapsedApps.has("appgroup-fw1")).toBe(true);
  });

  it("collapsed groups produce edge rewrites for all members", () => {
    const wf = makeWorkflow(
      [
        node("fw1", "FocusWindow", { method: "AppName", value: "Discord", bring_to_front: true }),
        node("c1", "Click"),
        node("t1", "TypeText", { text: "hi" }),
      ],
      [edge("fw1", "c1"), edge("c1", "t1")],
    );
    const { result } = renderHook(() => useAppGrouping(wf));
    const rewrites = result.current.collapsedAppEdgeRewrites;
    expect(rewrites.has("fw1")).toBe(true);
    expect(rewrites.has("c1")).toBe(true);
    expect(rewrites.has("t1")).toBe(true);
  });

  it("expanded groups have no edge rewrites", () => {
    const wf = makeWorkflow(
      [
        node("fw1", "FocusWindow", { method: "AppName", value: "Discord", bring_to_front: true }),
        node("c1", "Click"),
      ],
      [edge("fw1", "c1")],
    );
    const { result } = renderHook(() => useAppGrouping(wf));
    act(() => result.current.toggleAppCollapse("appgroup-fw1"));
    expect(result.current.collapsedAppEdgeRewrites.size).toBe(0);
  });

  it("collapsedAppEdgeRewrites maps members to anchor ID", () => {
    const wf = makeWorkflow(
      [
        node("fw1", "FocusWindow", { method: "AppName", value: "Discord", bring_to_front: true }),
        node("c1", "Click"),
        node("t1", "TypeText", { text: "hi" }),
      ],
      [edge("fw1", "c1"), edge("c1", "t1")],
    );
    const { result } = renderHook(() => useAppGrouping(wf));
    const rewrites = result.current.collapsedAppEdgeRewrites;
    expect(rewrites.get("fw1")).toBe("fw1");
    expect(rewrites.get("c1")).toBe("fw1");
    expect(rewrites.get("t1")).toBe("fw1");
  });

  it("nodes inside a loop are excluded from app groups (loop takes precedence)", () => {
    const wf = makeWorkflow(
      [
        node("fw1", "FocusWindow", { method: "AppName", value: "Discord", bring_to_front: true }),
        node("loop1", "Loop", { exit_condition: { type: "Always" }, max_iterations: 3 }),
        node("c1", "Click"),
        node("end1", "EndLoop", { loop_id: "loop1" }),
      ],
      [
        edge("fw1", "loop1"),
        edge("loop1", "c1", { type: "LoopBody" }),
        edge("c1", "end1"),
        edge("end1", "loop1"),
      ],
    );
    const { result } = renderHook(() => useAppGrouping(wf));
    const members = result.current.appGroups.get("appgroup-fw1") ?? [];
    expect(members).toContain("fw1");
    expect(members).toContain("loop1");
  });

  it("removed groups are cleaned from collapsed set", () => {
    const wf1 = makeWorkflow(
      [
        node("fw1", "FocusWindow", { method: "AppName", value: "Discord", bring_to_front: true }),
        node("c1", "Click"),
      ],
      [edge("fw1", "c1")],
    );
    const { result, rerender } = renderHook(
      ({ wf }) => useAppGrouping(wf),
      { initialProps: { wf: wf1 } },
    );
    expect(result.current.collapsedApps.has("appgroup-fw1")).toBe(true);

    const wf2 = makeWorkflow([], []);
    rerender({ wf: wf2 });
    expect(result.current.collapsedApps.has("appgroup-fw1")).toBe(false);
  });
});
