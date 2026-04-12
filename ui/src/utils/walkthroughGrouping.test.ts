import { describe, it, expect } from "vitest";
import {
  computeAppGroups,
  hashAppName,
  GROUP_COLORS,
  isValidItemDrop,
  buildInitialOrder,
} from "./walkthroughGrouping";
import type { Node, WalkthroughAction } from "../bindings";
import type { ActionNodeEntry } from "../store/slices/walkthroughSlice";

// --- factories ---

function makeKind(kindType: string, appName: string | null): WalkthroughAction["kind"] {
  switch (kindType) {
    case "FocusWindow": return { type: "FocusWindow", app_name: appName ?? "", window_title: null, app_kind: "Native" } as WalkthroughAction["kind"];
    case "LaunchApp": return { type: "LaunchApp", app_name: appName ?? "", app_kind: "Native" } as WalkthroughAction["kind"];
    case "Click": return { type: "Click", x: 0, y: 0, button: "Left", click_count: 1 } as WalkthroughAction["kind"];
    case "Hover": return { type: "Hover", x: 0, y: 0, dwell_ms: 500 } as WalkthroughAction["kind"];
    case "PressKey": return { type: "PressKey", key: "Return", modifiers: [] } as WalkthroughAction["kind"];
    case "TypeText": return { type: "TypeText", text: "hello" } as WalkthroughAction["kind"];
    case "Scroll": return { type: "Scroll", delta_y: -100 } as WalkthroughAction["kind"];
    default: return { type: "Click", x: 0, y: 0, button: "Left", click_count: 1 } as WalkthroughAction["kind"];
  }
}

function makeAction(id: string, appName: string | null, kindType = "Click", candidate = false) {
  return {
    id,
    kind: makeKind(kindType, appName),
    app_name: appName,
    window_title: null,
    target_candidates: [],
    artifact_paths: [],
    source_event_ids: [],
    confidence: "High" as const,
    warnings: [],
    screenshot_meta: null,
    candidate,
  };
}

function makeNode(id: string, type = "Click"): Node {
  return {
    id,
    node_type: { type, target: null, template_image: null, x: 0, y: 0 } as Node["node_type"],
    position: { x: 0, y: 0 },
    name: id,
    enabled: true,
    timeout_ms: null,
    settle_ms: null,
    retries: 0,
    trace_level: "Minimal",
    role: "Default",
    expected_outcome: null,
  };
}

// --- tests ---

describe("hashAppName", () => {
  it("returns consistent index for same name", () => {
    const a = hashAppName("Calculator");
    const b = hashAppName("Calculator");
    expect(a).toBe(b);
  });

  it("returns different indices for different names", () => {
    const a = hashAppName("Calculator");
    const b = hashAppName("Safari");
    // Could collide in theory, but these two shouldn't
    expect(a).not.toBe(b);
  });
});

describe("computeAppGroups", () => {
  it("groups consecutive same-app nodes", () => {
    const actions = [
      makeAction("a1", "Calculator", "FocusWindow"),
      makeAction("a2", "Calculator"),
      makeAction("a3", "Calculator"),
    ];
    const nodes = [makeNode("n1", "FocusWindow"), makeNode("n2"), makeNode("n3")];
    const map: ActionNodeEntry[] = [
      { action_id: "a1", node_id: "n1" },
      { action_id: "a2", node_id: "n2" },
      { action_id: "a3", node_id: "n3" },
    ];
    const order = ["n1", "n2", "n3"];

    const groups = computeAppGroups(order, nodes, actions, map);
    expect(groups).toHaveLength(1);
    expect(groups[0].appName).toBe("Calculator");
    expect(groups[0].items).toHaveLength(3);
    expect(groups[0].anchorIndices).toEqual(new Set([0]));
  });

  it("splits interleaved apps into separate groups", () => {
    const actions = [
      makeAction("a1", "Calculator", "FocusWindow"),
      makeAction("a2", "Calculator"),
      makeAction("a3", "Safari", "FocusWindow"),
      makeAction("a4", "Safari"),
    ];
    const nodes = [
      makeNode("n1", "FocusWindow"), makeNode("n2"),
      makeNode("n3", "FocusWindow"), makeNode("n4"),
    ];
    const map: ActionNodeEntry[] = [
      { action_id: "a1", node_id: "n1" },
      { action_id: "a2", node_id: "n2" },
      { action_id: "a3", node_id: "n3" },
      { action_id: "a4", node_id: "n4" },
    ];
    const order = ["n1", "n2", "n3", "n4"];

    const groups = computeAppGroups(order, nodes, actions, map);
    expect(groups).toHaveLength(2);
    expect(groups[0].appName).toBe("Calculator");
    expect(groups[0].items).toHaveLength(2);
    expect(groups[1].appName).toBe("Safari");
    expect(groups[1].items).toHaveLength(2);
  });

  it("ungrouped items (no app_name) become singleton groups with null appName", () => {
    const actions = [
      makeAction("a1", "Calculator", "FocusWindow"),
      makeAction("a2", "Calculator"),
      makeAction("a3", null, "PressKey"),
    ];
    const nodes = [makeNode("n1", "FocusWindow"), makeNode("n2"), makeNode("n3", "PressKey")];
    const map: ActionNodeEntry[] = [
      { action_id: "a1", node_id: "n1" },
      { action_id: "a2", node_id: "n2" },
      { action_id: "a3", node_id: "n3" },
    ];
    const order = ["n1", "n2", "n3"];

    const groups = computeAppGroups(order, nodes, actions, map);
    expect(groups).toHaveLength(2);
    expect(groups[0].appName).toBe("Calculator");
    expect(groups[1].appName).toBeNull();
    expect(groups[1].items).toHaveLength(1);
  });

  it("includes candidate actions in groups", () => {
    const actions = [
      makeAction("a1", "Calculator", "FocusWindow"),
      makeAction("a2", "Calculator"),
      makeAction("a3", "Calculator", "Hover", true), // candidate
    ];
    const nodes = [makeNode("n1", "FocusWindow"), makeNode("n2")];
    const map: ActionNodeEntry[] = [
      { action_id: "a1", node_id: "n1" },
      { action_id: "a2", node_id: "n2" },
    ];
    // Candidate uses action ID in order
    const order = ["n1", "n2", "a3"];

    const groups = computeAppGroups(order, nodes, actions, map);
    expect(groups).toHaveLength(1);
    expect(groups[0].items).toHaveLength(3);
    expect(groups[0].items[2].type).toBe("candidate");
  });

  it("skips stale IDs not found in nodes or actions", () => {
    const actions = [makeAction("a1", "Calculator", "FocusWindow")];
    const nodes = [makeNode("n1", "FocusWindow")];
    const map: ActionNodeEntry[] = [{ action_id: "a1", node_id: "n1" }];
    const order = ["n1", "stale-id"];

    const groups = computeAppGroups(order, nodes, actions, map);
    expect(groups).toHaveLength(1);
    expect(groups[0].items).toHaveLength(1);
  });
});

describe("isValidItemDrop", () => {
  // Build a simple group: FocusWindow anchor at index 0, two actions after
  const actions = [
    makeAction("a1", "Calculator", "FocusWindow"),
    makeAction("a2", "Calculator"),
    makeAction("a3", "Calculator"),
  ];
  const nodes = [makeNode("n1", "FocusWindow"), makeNode("n2"), makeNode("n3")];
  const map: ActionNodeEntry[] = [
    { action_id: "a1", node_id: "n1" },
    { action_id: "a2", node_id: "n2" },
    { action_id: "a3", node_id: "n3" },
  ];
  const order = ["n1", "n2", "n3"];

  it("allows reordering non-anchor items within group", () => {
    const groups = computeAppGroups(order, nodes, actions, map);
    // Drag n3 to position before n2 (but after anchor n1) → valid
    expect(isValidItemDrop("n3", 1, groups)).toBe(true);
  });

  it("rejects dropping a non-anchor item above the anchor", () => {
    const groups = computeAppGroups(order, nodes, actions, map);
    // Drag n2 to position 0 (above FocusWindow n1) → invalid
    expect(isValidItemDrop("n2", 0, groups)).toBe(false);
  });

  it("rejects dragging the anchor item itself", () => {
    const groups = computeAppGroups(order, nodes, actions, map);
    // FocusWindow anchor can't be dragged at all
    expect(isValidItemDrop("n1", 2, groups)).toBe(false);
  });
});

describe("buildInitialOrder", () => {
  it("interleaves candidates into correct chronological position", () => {
    const actions = [
      makeAction("a1", "Calculator", "FocusWindow"),
      makeAction("a2", "Calculator"),
      makeAction("a3", "Calculator", "Hover", true), // candidate
      makeAction("a4", "Safari", "FocusWindow"),
    ];
    const nodes = [
      makeNode("n1", "FocusWindow"), makeNode("n2"),
      makeNode("n4", "FocusWindow"),
    ];
    const map: ActionNodeEntry[] = [
      { action_id: "a1", node_id: "n1" },
      { action_id: "a2", node_id: "n2" },
      { action_id: "a4", node_id: "n4" },
    ];

    const order = buildInitialOrder(actions, nodes, map);
    expect(order).toEqual(["n1", "n2", "a3", "n4"]);
  });
});

// --- applyAnnotationsToDraft with nodeOrder ---

import { applyAnnotationsToDraft } from "./walkthroughDraft";

describe("applyAnnotationsToDraft with nodeOrder", () => {
  it("reorders nodes and rebuilds edges in new order", () => {
    const draft = {
      id: "wf-1",
      name: "test",
      nodes: [makeNode("n1"), makeNode("n2"), makeNode("n3")],
      edges: [],
      groups: [],
    };
    const annotations = {
      deleted_node_ids: [],
      renamed_nodes: [],
      target_overrides: [],
      variable_promotions: [],
    };
    const result = applyAnnotationsToDraft(
      draft, annotations, [], [],
      ["n3", "n1", "n2"], // reordered
    );
    expect(result.nodes.map((n) => n.id)).toEqual(["n3", "n1", "n2"]);
    expect(result.edges).toEqual([
      { from: "n3", to: "n1", output: null },
      { from: "n1", to: "n2", output: null },
    ]);
  });

  it("uses original order when nodeOrder is undefined", () => {
    const draft = {
      id: "wf-1",
      name: "test",
      nodes: [makeNode("n1"), makeNode("n2")],
      edges: [],
      groups: [],
    };
    const annotations = {
      deleted_node_ids: [],
      renamed_nodes: [],
      target_overrides: [],
      variable_promotions: [],
    };
    const result = applyAnnotationsToDraft(draft, annotations, [], []);
    expect(result.nodes.map((n) => n.id)).toEqual(["n1", "n2"]);
  });
});
