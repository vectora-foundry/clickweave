import { describe, it, expect } from "vitest";
import { renderHook, act } from "@testing-library/react";
import { useUserGrouping } from "./useUserGrouping";
import type { NodeGroup } from "../bindings";
import { node, edge, makeWorkflow } from "./test-helpers";

function group(
  id: string,
  name: string,
  nodeIds: string[],
  parentGroupId?: string,
): NodeGroup {
  return {
    id,
    name,
    color: "#6366f1",
    node_ids: nodeIds,
    parent_group_id: parentGroupId ?? null,
  };
}

describe("useUserGrouping", () => {
  it("computes nodeToUserGroup from workflow.groups", () => {
    const wf = makeWorkflow(
      [node("a", "Click"), node("b", "Click"), node("c", "Click")],
      [edge("a", "b"), edge("b", "c")],
      [group("g1", "Group 1", ["a", "b", "c"])],
    );
    const { result } = renderHook(() => useUserGrouping(wf));
    expect(result.current.nodeToUserGroup.get("a")).toBe("g1");
    expect(result.current.nodeToUserGroup.get("b")).toBe("g1");
    expect(result.current.nodeToUserGroup.get("c")).toBe("g1");
  });

  it("new groups default to expanded (not in collapsedUserGroups)", () => {
    const wf = makeWorkflow(
      [node("a", "Click"), node("b", "Click")],
      [edge("a", "b")],
      [group("g1", "Group 1", ["a", "b"])],
    );
    const { result } = renderHook(() => useUserGrouping(wf));
    expect(result.current.collapsedUserGroups.has("g1")).toBe(false);
  });

  it("toggleUserGroupCollapse adds group to collapsed set", () => {
    const wf = makeWorkflow(
      [node("a", "Click"), node("b", "Click")],
      [edge("a", "b")],
      [group("g1", "Group 1", ["a", "b"])],
    );
    const { result } = renderHook(() => useUserGrouping(wf));
    expect(result.current.collapsedUserGroups.has("g1")).toBe(false);

    act(() => result.current.toggleUserGroupCollapse("g1"));
    expect(result.current.collapsedUserGroups.has("g1")).toBe(true);
  });

  it("toggleUserGroupCollapse removes group from collapsed set when already collapsed", () => {
    const wf = makeWorkflow(
      [node("a", "Click"), node("b", "Click")],
      [edge("a", "b")],
      [group("g1", "Group 1", ["a", "b"])],
    );
    const { result } = renderHook(() => useUserGrouping(wf));

    act(() => result.current.toggleUserGroupCollapse("g1"));
    expect(result.current.collapsedUserGroups.has("g1")).toBe(true);

    act(() => result.current.toggleUserGroupCollapse("g1"));
    expect(result.current.collapsedUserGroups.has("g1")).toBe(false);
  });

  it("computes edge rewrites for collapsed groups (maps all members to anchor)", () => {
    const wf = makeWorkflow(
      [node("a", "Click"), node("b", "Click"), node("c", "Click")],
      [edge("a", "b"), edge("b", "c")],
      [group("g1", "Group 1", ["a", "b", "c"])],
    );
    const { result } = renderHook(() => useUserGrouping(wf));

    act(() => result.current.toggleUserGroupCollapse("g1"));

    const rewrites = result.current.userGroupEdgeRewrites;
    expect(rewrites.get("a")).toBe("a"); // anchor maps to itself
    expect(rewrites.get("b")).toBe("a");
    expect(rewrites.get("c")).toBe("a");
  });

  it("expanded groups have no edge rewrites", () => {
    const wf = makeWorkflow(
      [node("a", "Click"), node("b", "Click")],
      [edge("a", "b")],
      [group("g1", "Group 1", ["a", "b"])],
    );
    const { result } = renderHook(() => useUserGrouping(wf));
    expect(result.current.userGroupEdgeRewrites.size).toBe(0);
  });

  it("computes hidden node IDs for collapsed groups (non-anchor members hidden, anchor NOT hidden)", () => {
    const wf = makeWorkflow(
      [node("a", "Click"), node("b", "Click"), node("c", "Click")],
      [edge("a", "b"), edge("b", "c")],
      [group("g1", "Group 1", ["a", "b", "c"])],
    );
    const { result } = renderHook(() => useUserGrouping(wf));

    act(() => result.current.toggleUserGroupCollapse("g1"));

    const hidden = result.current.hiddenUserGroupNodeIds;
    expect(hidden.has("a")).toBe(false); // anchor is NOT hidden
    expect(hidden.has("b")).toBe(true);
    expect(hidden.has("c")).toBe(true);
  });

  it("expanded groups have no hidden nodes", () => {
    const wf = makeWorkflow(
      [node("a", "Click"), node("b", "Click")],
      [edge("a", "b")],
      [group("g1", "Group 1", ["a", "b"])],
    );
    const { result } = renderHook(() => useUserGrouping(wf));
    expect(result.current.hiddenUserGroupNodeIds.size).toBe(0);
  });

  it("computes userGroupMeta with name, color, anchorId", () => {
    const wf = makeWorkflow(
      [node("a", "Click"), node("b", "Click")],
      [edge("a", "b")],
      [group("g1", "My Group", ["a", "b"])],
    );
    const { result } = renderHook(() => useUserGrouping(wf));
    const meta = result.current.userGroupMeta.get("g1");
    expect(meta?.name).toBe("My Group");
    expect(meta?.color).toBe("#6366f1");
    expect(meta?.anchorId).toBe("a");
    expect(meta?.parentGroupId).toBeNull();
  });

  it("returns flat member count including subgroup members", () => {
    const wf = makeWorkflow(
      [node("a", "Click"), node("b", "Click"), node("c", "Click"), node("d", "Click")],
      [edge("a", "b"), edge("b", "c"), edge("c", "d")],
      [
        group("parent", "Parent Group", ["a", "b", "c", "d"]),
        group("child", "Child Group", ["b", "c"], "parent"),
      ],
    );
    const { result } = renderHook(() => useUserGrouping(wf));
    const parentMeta = result.current.userGroupMeta.get("parent");
    const childMeta = result.current.userGroupMeta.get("child");
    expect(parentMeta?.flatMemberCount).toBe(4);
    expect(childMeta?.flatMemberCount).toBe(2);
  });

  it("collapsed parent hides all members except parent anchor, even if member is subgroup anchor", () => {
    const wf = makeWorkflow(
      [node("a", "Click"), node("b", "Click"), node("c", "Click"), node("d", "Click")],
      [edge("a", "b"), edge("b", "c"), edge("c", "d")],
      [
        group("parent", "Parent Group", ["a", "b", "c", "d"]),
        group("child", "Child Group", ["b", "c"], "parent"),
      ],
    );
    const { result } = renderHook(() => useUserGrouping(wf));

    act(() => result.current.toggleUserGroupCollapse("parent"));

    const hidden = result.current.hiddenUserGroupNodeIds;
    expect(hidden.has("a")).toBe(false); // parent anchor: NOT hidden
    expect(hidden.has("b")).toBe(true);  // subgroup anchor, but parent collapsed: hidden
    expect(hidden.has("c")).toBe(true);
    expect(hidden.has("d")).toBe(true);
  });

  it("collapsed child with expanded parent only hides child non-anchor members", () => {
    const wf = makeWorkflow(
      [node("a", "Click"), node("b", "Click"), node("c", "Click"), node("d", "Click")],
      [edge("a", "b"), edge("b", "c"), edge("c", "d")],
      [
        group("parent", "Parent Group", ["a", "b", "c", "d"]),
        group("child", "Child Group", ["b", "c"], "parent"),
      ],
    );
    const { result } = renderHook(() => useUserGrouping(wf));

    act(() => result.current.toggleUserGroupCollapse("child"));

    const hidden = result.current.hiddenUserGroupNodeIds;
    expect(hidden.has("a")).toBe(false);
    expect(hidden.has("b")).toBe(false); // child anchor: NOT hidden
    expect(hidden.has("c")).toBe(true);  // child non-anchor: hidden
    expect(hidden.has("d")).toBe(false);
  });

  it("subgroup edge rewrites skipped when parent is also collapsed (parent takes precedence)", () => {
    const wf = makeWorkflow(
      [node("a", "Click"), node("b", "Click"), node("c", "Click"), node("d", "Click")],
      [edge("a", "b"), edge("b", "c"), edge("c", "d")],
      [
        group("parent", "Parent Group", ["a", "b", "c", "d"]),
        group("child", "Child Group", ["b", "c"], "parent"),
      ],
    );
    const { result } = renderHook(() => useUserGrouping(wf));

    // Collapse both parent and child
    act(() => {
      result.current.toggleUserGroupCollapse("parent");
      result.current.toggleUserGroupCollapse("child");
    });

    const rewrites = result.current.userGroupEdgeRewrites;
    // Parent is collapsed: all its members should rewrite to "a"
    expect(rewrites.get("a")).toBe("a");
    expect(rewrites.get("b")).toBe("a");
    expect(rewrites.get("c")).toBe("a");
    expect(rewrites.get("d")).toBe("a");
    // Child should NOT overwrite with its own anchor "b" since parent takes precedence
    // All rewrites should be to "a" (parent anchor), not "b" (child anchor)
  });

  it("removed groups are cleaned from collapsed set", () => {
    const wf1 = makeWorkflow(
      [node("a", "Click"), node("b", "Click")],
      [edge("a", "b")],
      [group("g1", "Group 1", ["a", "b"])],
    );
    const { result, rerender } = renderHook(
      ({ wf }) => useUserGrouping(wf),
      { initialProps: { wf: wf1 } },
    );

    act(() => result.current.toggleUserGroupCollapse("g1"));
    expect(result.current.collapsedUserGroups.has("g1")).toBe(true);

    const wf2 = makeWorkflow([], [], []);
    rerender({ wf: wf2 });
    expect(result.current.collapsedUserGroups.has("g1")).toBe(false);
  });

  it("workflow with no groups returns empty maps", () => {
    const wf = makeWorkflow([node("a", "Click")], [], []);
    const { result } = renderHook(() => useUserGrouping(wf));
    expect(result.current.nodeToUserGroup.size).toBe(0);
    expect(result.current.userGroupMeta.size).toBe(0);
    expect(result.current.collapsedUserGroups.size).toBe(0);
    expect(result.current.userGroupEdgeRewrites.size).toBe(0);
    expect(result.current.hiddenUserGroupNodeIds.size).toBe(0);
  });
});
