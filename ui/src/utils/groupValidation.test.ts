import { describe, it, expect } from "vitest";
import {
  isConnectedSubgraph,
  validateGroupCreation,
  topologicalSortMembers,
} from "./groupValidation";
import { node, edge, makeWorkflow } from "../hooks/test-helpers";

// ─── isConnectedSubgraph ─────────────────────────────────────────────────────

describe("isConnectedSubgraph", () => {
  it("returns true for adjacent nodes", () => {
    const workflow = makeWorkflow(
      [node("a", "AiStep"), node("b", "AiStep")],
      [edge("a", "b")],
    );
    expect(isConnectedSubgraph(["a", "b"], workflow)).toBe(true);
  });

  it("returns false for disconnected nodes", () => {
    const workflow = makeWorkflow(
      [node("a", "AiStep"), node("b", "AiStep"), node("c", "AiStep")],
      [edge("a", "b")],
    );
    expect(isConnectedSubgraph(["a", "c"], workflow)).toBe(false);
  });

  it("returns true for branch selection (A->B, A->C, selecting A and B)", () => {
    const workflow = makeWorkflow(
      [node("a", "AiStep"), node("b", "AiStep"), node("c", "AiStep")],
      [edge("a", "b"), edge("a", "c")],
    );
    // A and B are connected via the A->B edge
    expect(isConnectedSubgraph(["a", "b"], workflow)).toBe(true);
  });

  it("returns false for a single node", () => {
    const workflow = makeWorkflow([node("a", "AiStep")], []);
    expect(isConnectedSubgraph(["a"], workflow)).toBe(false);
  });

  it("returns true for three connected nodes in a chain", () => {
    const workflow = makeWorkflow(
      [node("a", "AiStep"), node("b", "AiStep"), node("c", "AiStep")],
      [edge("a", "b"), edge("b", "c")],
    );
    expect(isConnectedSubgraph(["a", "b", "c"], workflow)).toBe(true);
  });

  it("returns false for three nodes where only two are connected within the subgraph", () => {
    // a->b, b->c, d->c — selecting a,b,d: d has no edge to a or b
    const workflow = makeWorkflow(
      [node("a", "AiStep"), node("b", "AiStep"), node("c", "AiStep"), node("d", "AiStep")],
      [edge("a", "b"), edge("b", "c"), edge("d", "c")],
    );
    expect(isConnectedSubgraph(["a", "b", "d"], workflow)).toBe(false);
  });
});

// ─── validateGroupCreation ───────────────────────────────────────────────────

describe("validateGroupCreation", () => {
  it("rejects fewer than 2 nodes", () => {
    const workflow = makeWorkflow([node("a", "AiStep")], []);
    const result = validateGroupCreation(["a"], workflow, [], new Map(), new Map());
    expect(result.valid).toBe(false);
    expect(result.error).toMatch(/at least 2/i);
  });

  it("rejects disconnected selection", () => {
    const workflow = makeWorkflow(
      [node("a", "AiStep"), node("b", "AiStep"), node("c", "AiStep")],
      [edge("a", "b")],
    );
    const result = validateGroupCreation(["a", "c"], workflow, [], new Map(), new Map());
    expect(result.valid).toBe(false);
    expect(result.error).toMatch(/connected/i);
  });

  it("rejects partial overlap with existing user group", () => {
    const existingGroup = {
      id: "g1",
      name: "Group 1",
      color: "#ff0000",
      node_ids: ["a", "b", "c"],
      parent_group_id: null,
    };
    const workflow = makeWorkflow(
      [node("a", "AiStep"), node("b", "AiStep"), node("c", "AiStep"), node("d", "AiStep")],
      [edge("a", "b"), edge("b", "c"), edge("c", "d")],
      [existingGroup],
    );
    // Selecting b,c,d — b and c are in g1 but d is not
    const result = validateGroupCreation(
      ["b", "c", "d"],
      workflow,
      [existingGroup],
      new Map(),
      new Map(),
    );
    expect(result.valid).toBe(false);
    expect(result.error).toMatch(/overlap/i);
  });

  it("allows wrapping an entire existing user group", () => {
    const existingGroup = {
      id: "g1",
      name: "Group 1",
      color: "#ff0000",
      node_ids: ["b", "c"],
      parent_group_id: null,
    };
    const workflow = makeWorkflow(
      [node("a", "AiStep"), node("b", "AiStep"), node("c", "AiStep"), node("d", "AiStep")],
      [edge("a", "b"), edge("b", "c"), edge("c", "d")],
      [existingGroup],
    );
    // Selecting a,b,c,d fully contains g1
    const result = validateGroupCreation(
      ["a", "b", "c", "d"],
      workflow,
      [existingGroup],
      new Map(),
      new Map(),
    );
    expect(result.valid).toBe(true);
  });

  it("rejects nesting beyond 2 levels", () => {
    // g1 is already a subgroup (has a parent)
    const parentGroup = {
      id: "parent",
      name: "Parent",
      color: "#0000ff",
      node_ids: ["a", "b", "c"],
      parent_group_id: null,
    };
    const subGroup = {
      id: "g1",
      name: "Sub Group",
      color: "#ff0000",
      node_ids: ["b", "c"],
      parent_group_id: "parent",
    };
    const workflow = makeWorkflow(
      [node("a", "AiStep"), node("b", "AiStep"), node("c", "AiStep")],
      [edge("a", "b"), edge("b", "c")],
      [parentGroup, subGroup],
    );
    // Trying to create a group inside g1 (which is already a subgroup)
    const result = validateGroupCreation(
      ["b", "c"],
      workflow,
      [parentGroup, subGroup],
      new Map(),
      new Map(),
    );
    expect(result.valid).toBe(false);
    expect(result.error).toMatch(/nesting/i);
  });

  it("accepts valid connected selection", () => {
    const workflow = makeWorkflow(
      [node("a", "AiStep"), node("b", "AiStep"), node("c", "AiStep")],
      [edge("a", "b"), edge("b", "c")],
    );
    const result = validateGroupCreation(["a", "b"], workflow, [], new Map(), new Map());
    expect(result.valid).toBe(true);
    expect(result.parentGroupId).toBeUndefined();
  });

  it("rejects partial overlap with loop auto-group", () => {
    // loopId "loop1" owns nodes a, b, c; we try to select b and d (partial overlap)
    const loopMembers = new Map([["loop1", ["a", "b", "c"]]]);
    const workflow = makeWorkflow(
      [node("a", "AiStep"), node("b", "AiStep"), node("c", "AiStep"), node("d", "AiStep")],
      [edge("a", "b"), edge("b", "c"), edge("c", "d")],
    );
    const result = validateGroupCreation(["b", "c", "d"], workflow, [], loopMembers, new Map());
    expect(result.valid).toBe(false);
    expect(result.error).toMatch(/overlap/i);
  });

  it("rejects partial overlap with app auto-group", () => {
    // appGroup "app1" owns nodes a, b; we try to select b and c (b is in the
    // group, c is not — straddles the boundary; b->c edge makes it connected)
    const appGroups = new Map([["app1", ["a", "b"]]]);
    const workflow = makeWorkflow(
      [node("a", "AiStep"), node("b", "AiStep"), node("c", "AiStep")],
      [edge("a", "b"), edge("b", "c")],
    );
    const result = validateGroupCreation(["b", "c"], workflow, [], new Map(), appGroups);
    expect(result.valid).toBe(false);
    expect(result.error).toMatch(/overlap/i);
  });

  it("allows wrapping entire auto-group", () => {
    const loopMembers = new Map([["loop1", ["b", "c"]]]);
    const workflow = makeWorkflow(
      [node("a", "AiStep"), node("b", "AiStep"), node("c", "AiStep"), node("d", "AiStep")],
      [edge("a", "b"), edge("b", "c"), edge("c", "d")],
    );
    // Selecting all 4 nodes fully contains the loop
    const result = validateGroupCreation(
      ["a", "b", "c", "d"],
      workflow,
      [],
      loopMembers,
      new Map(),
    );
    expect(result.valid).toBe(true);
  });

  it("allows grouping inside an auto-group (all selected nodes within auto-group)", () => {
    // appGroup "app1" owns a,b,c — selecting a,b (subset of auto-group) should be valid
    const appGroups = new Map([["app1", ["a", "b", "c"]]]);
    const workflow = makeWorkflow(
      [node("a", "AiStep"), node("b", "AiStep"), node("c", "AiStep")],
      [edge("a", "b"), edge("b", "c")],
    );
    const result = validateGroupCreation(["a", "b"], workflow, [], new Map(), appGroups);
    expect(result.valid).toBe(true);
  });
});

// ─── topologicalSortMembers ──────────────────────────────────────────────────

describe("topologicalSortMembers", () => {
  it("sorts a simple chain", () => {
    const workflow = makeWorkflow(
      [node("a", "AiStep"), node("b", "AiStep"), node("c", "AiStep")],
      [edge("a", "b"), edge("b", "c")],
    );
    expect(topologicalSortMembers(["c", "a", "b"], workflow)).toEqual(["a", "b", "c"]);
  });

  it("handles branching DAG", () => {
    // a -> b, a -> c (b and c are parallel)
    const workflow = makeWorkflow(
      [node("a", "AiStep"), node("b", "AiStep"), node("c", "AiStep")],
      [edge("a", "b"), edge("a", "c")],
    );
    const sorted = topologicalSortMembers(["b", "c", "a"], workflow);
    // a must come first
    expect(sorted[0]).toBe("a");
    // b and c can be in either order after a
    expect(sorted).toHaveLength(3);
    expect(new Set(sorted)).toEqual(new Set(["a", "b", "c"]));
  });

  it("handles nodes with no internal edges", () => {
    // No edges at all among the three nodes
    const workflow = makeWorkflow(
      [node("a", "AiStep"), node("b", "AiStep"), node("c", "AiStep")],
      [],
    );
    const sorted = topologicalSortMembers(["b", "a", "c"], workflow);
    expect(sorted).toHaveLength(3);
    expect(new Set(sorted)).toEqual(new Set(["a", "b", "c"]));
  });

  it("ignores edges that cross outside the member set", () => {
    // a -> b -> c -> d, but we only sort a, b, c
    const workflow = makeWorkflow(
      [
        node("a", "AiStep"),
        node("b", "AiStep"),
        node("c", "AiStep"),
        node("d", "AiStep"),
      ],
      [edge("a", "b"), edge("b", "c"), edge("c", "d")],
    );
    expect(topologicalSortMembers(["c", "b", "a"], workflow)).toEqual(["a", "b", "c"]);
  });
});
