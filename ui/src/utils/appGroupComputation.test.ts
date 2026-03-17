import { describe, it, expect } from "vitest";
import { buildAppNameMap, computeAppMembers } from "./appGroupComputation";
import { node, edge, makeWorkflow } from "../hooks/test-helpers";

describe("buildAppNameMap", () => {
  it("propagates app name from AppName FocusWindow downstream", () => {
    const wf = makeWorkflow(
      [
        node("fw1", "FocusWindow", { method: "AppName", value: "Discord", bring_to_front: true }),
        node("c1", "Click"),
        node("t1", "TypeText", { text: "hello" }),
      ],
      [edge("fw1", "c1"), edge("c1", "t1")],
    );
    const map = buildAppNameMap(wf);
    expect(map.get("fw1")).toBe("Discord");
    expect(map.get("c1")).toBe("Discord");
    expect(map.get("t1")).toBe("Discord");
  });

  it("resets app name at non-AppName FocusWindow", () => {
    const wf = makeWorkflow(
      [
        node("fw1", "FocusWindow", { method: "AppName", value: "Discord", bring_to_front: true }),
        node("c1", "Click"),
        node("fw2", "FocusWindow", { method: "Pid", value: "1234", bring_to_front: true }),
        node("c2", "Click"),
      ],
      [edge("fw1", "c1"), edge("c1", "fw2"), edge("fw2", "c2")],
    );
    const map = buildAppNameMap(wf);
    expect(map.get("fw1")).toBe("Discord");
    expect(map.get("c1")).toBe("Discord");
    expect(map.get("fw2")).toBeNull();
    expect(map.get("c2")).toBeNull();
  });

  it("new AppName FocusWindow overrides previous app name", () => {
    const wf = makeWorkflow(
      [
        node("fw1", "FocusWindow", { method: "AppName", value: "Discord", bring_to_front: true }),
        node("c1", "Click"),
        node("fw2", "FocusWindow", { method: "AppName", value: "Signal", bring_to_front: true }),
        node("c2", "Click"),
      ],
      [edge("fw1", "c1"), edge("c1", "fw2"), edge("fw2", "c2")],
    );
    const map = buildAppNameMap(wf);
    expect(map.get("c1")).toBe("Discord");
    expect(map.get("fw2")).toBe("Signal");
    expect(map.get("c2")).toBe("Signal");
  });

  it("nodes with no upstream FocusWindow have no entry", () => {
    const wf = makeWorkflow(
      [node("a", "AiStep"), node("b", "Click")],
      [edge("a", "b")],
    );
    const map = buildAppNameMap(wf);
    expect(map.has("a")).toBe(false);
    expect(map.has("b")).toBe(false);
  });

  it("excludes EndLoop back-edges from propagation", () => {
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
        edge("loop1", "fw1", { type: "LoopDone" }),
      ],
    );
    const map = buildAppNameMap(wf);
    expect(map.get("loop1")).toBe("Discord");
    expect(map.get("c1")).toBe("Discord");
  });
});

describe("computeAppMembers", () => {
  it("groups consecutive same-app nodes with synthetic group ID", () => {
    const wf = makeWorkflow(
      [
        node("fw1", "FocusWindow", { method: "AppName", value: "Discord", bring_to_front: true }),
        node("c1", "Click"),
        node("t1", "TypeText", { text: "hi" }),
      ],
      [edge("fw1", "c1"), edge("c1", "t1")],
    );
    const nameMap = buildAppNameMap(wf);
    const groups = computeAppMembers(wf, nameMap);
    expect(groups.size).toBe(1);
    const groupId = "appgroup-fw1";
    expect(groups.has(groupId)).toBe(true);
    expect(groups.get(groupId)).toEqual(["fw1", "c1", "t1"]);
  });

  it("creates separate groups when app changes", () => {
    const wf = makeWorkflow(
      [
        node("fw1", "FocusWindow", { method: "AppName", value: "Discord", bring_to_front: true }),
        node("c1", "Click"),
        node("fw2", "FocusWindow", { method: "AppName", value: "Signal", bring_to_front: true }),
        node("c2", "Click"),
      ],
      [edge("fw1", "c1"), edge("c1", "fw2"), edge("fw2", "c2")],
    );
    const nameMap = buildAppNameMap(wf);
    const groups = computeAppMembers(wf, nameMap);
    expect(groups.size).toBe(2);
    expect(groups.get("appgroup-fw1")).toEqual(["fw1", "c1"]);
    expect(groups.get("appgroup-fw2")).toEqual(["fw2", "c2"]);
  });

  it("ungrouped nodes between app groups are excluded", () => {
    const wf = makeWorkflow(
      [
        node("fw1", "FocusWindow", { method: "AppName", value: "Discord", bring_to_front: true }),
        node("c1", "Click"),
        node("if1", "If", { condition: { type: "Always" } }),
        node("fw2", "FocusWindow", { method: "AppName", value: "Signal", bring_to_front: true }),
        node("c2", "Click"),
      ],
      [edge("fw1", "c1"), edge("c1", "if1"), edge("if1", "fw2"), edge("fw2", "c2")],
    );
    const nameMap = buildAppNameMap(wf);
    const groups = computeAppMembers(wf, nameMap);
    expect(groups.get("appgroup-fw1")).toEqual(["fw1", "c1", "if1"]);
    expect(groups.get("appgroup-fw2")).toEqual(["fw2", "c2"]);
  });

  it("does not group nodes with no app name", () => {
    const wf = makeWorkflow(
      [node("a", "AiStep"), node("b", "Click")],
      [edge("a", "b")],
    );
    const nameMap = buildAppNameMap(wf);
    const groups = computeAppMembers(wf, nameMap);
    expect(groups.size).toBe(0);
  });

  it("same app returning after different app creates a new group", () => {
    const wf = makeWorkflow(
      [
        node("fw1", "FocusWindow", { method: "AppName", value: "Discord", bring_to_front: true }),
        node("c1", "Click"),
        node("fw2", "FocusWindow", { method: "AppName", value: "Signal", bring_to_front: true }),
        node("c2", "Click"),
        node("fw3", "FocusWindow", { method: "AppName", value: "Discord", bring_to_front: true }),
        node("c3", "Click"),
      ],
      [
        edge("fw1", "c1"), edge("c1", "fw2"), edge("fw2", "c2"),
        edge("c2", "fw3"), edge("fw3", "c3"),
      ],
    );
    const nameMap = buildAppNameMap(wf);
    const groups = computeAppMembers(wf, nameMap);
    expect(groups.size).toBe(3);
    expect(groups.get("appgroup-fw1")).toEqual(["fw1", "c1"]);
    expect(groups.get("appgroup-fw2")).toEqual(["fw2", "c2"]);
    expect(groups.get("appgroup-fw3")).toEqual(["fw3", "c3"]);
  });

  it("single FocusWindow with no children still forms a group", () => {
    const wf = makeWorkflow(
      [node("fw1", "FocusWindow", { method: "AppName", value: "Discord", bring_to_front: true })],
      [],
    );
    const nameMap = buildAppNameMap(wf);
    const groups = computeAppMembers(wf, nameMap);
    expect(groups.get("appgroup-fw1")).toEqual(["fw1"]);
  });
});
