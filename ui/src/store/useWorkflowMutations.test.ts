import { describe, it, expect } from "vitest";
import { autoDissolveGroups } from "./useWorkflowMutations";
import type { NodeGroup } from "../bindings";

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

describe("autoDissolveGroups", () => {
  it("keeps groups with 2+ direct members", () => {
    const groups = [group("g1", "G1", ["a", "b"])];
    expect(autoDissolveGroups(groups)).toHaveLength(1);
  });

  it("dissolves groups with fewer than 2 members", () => {
    const groups = [group("g1", "G1", ["a"])];
    expect(autoDissolveGroups(groups)).toHaveLength(0);
  });

  it("dissolves empty groups", () => {
    const groups = [group("g1", "G1", [])];
    expect(autoDissolveGroups(groups)).toHaveLength(0);
  });

  it("counts subgroups as single items — parent with 2 subgroups survives", () => {
    const groups = [
      group("parent", "Parent", ["a", "b", "c", "d"]),
      group("child1", "Child 1", ["a", "b"], "parent"),
      group("child2", "Child 2", ["c", "d"], "parent"),
    ];
    const result = autoDissolveGroups(groups);
    expect(result).toHaveLength(3);
    expect(result.find((g) => g.id === "parent")).not.toBeUndefined();
  });

  it("dissolves parent with only 1 subgroup and no direct members, promotes child", () => {
    const groups = [
      group("parent", "Parent", ["a", "b"]),
      group("child", "Child", ["a", "b"], "parent"),
    ];
    const result = autoDissolveGroups(groups);
    // Parent has 0 direct members + 1 subgroup = 1 effective member → dissolves
    // Child promoted to top-level, still has 2 members → survives
    expect(result).toHaveLength(1);
    expect(result[0]!.id).toBe("child");
    expect(result[0]!.parent_group_id).toBeNull();
  });

  it("keeps parent with 1 subgroup and 1+ direct member", () => {
    const groups = [
      group("parent", "Parent", ["a", "b", "c"]),
      group("child", "Child", ["a", "b"], "parent"),
    ];
    const result = autoDissolveGroups(groups);
    // Parent has 1 direct member (c) + 1 subgroup = 2 effective → survives
    expect(result).toHaveLength(2);
  });

  it("cascade: dissolving a child reduces parent effective count", () => {
    // child2 has only 1 member → dissolves.
    // After child2 dissolves, parent has 1 direct member (c) + 1 subgroup = 2 → survives.
    const groups = [
      group("parent", "Parent", ["a", "b", "c"]),
      group("child1", "Child 1", ["a", "b"], "parent"),
      group("child2", "Child 2", ["c"], "parent"),
    ];
    const result = autoDissolveGroups(groups);
    expect(result).toHaveLength(2);
    expect(result.map((g) => g.id).sort()).toEqual(["child1", "parent"]);
  });

  it("promotes orphaned subgroups to top-level when parent dissolves", () => {
    // Parent has 1 member → dissolves
    // Child has parent_group_id "parent" → promoted to top-level (null parent)
    // But child also has only 1 member → dissolves too
    const groups = [
      group("parent", "Parent", ["a"]),
      group("child", "Child", ["a"], "parent"),
    ];
    const result = autoDissolveGroups(groups);
    expect(result).toHaveLength(0);
  });

  it("promotes orphaned subgroup with enough members to top-level", () => {
    // Parent has 0 direct + 1 subgroup = 1 effective → dissolves
    // Child has 2 members, promoted to top-level → survives
    const groups = [
      group("parent", "Parent", ["a", "b"]),
      group("child", "Child", ["a", "b"], "parent"),
    ];
    const result = autoDissolveGroups(groups);
    expect(result).toHaveLength(1);
    expect(result[0]!.id).toBe("child");
    expect(result[0]!.parent_group_id).toBeNull();
  });
});
