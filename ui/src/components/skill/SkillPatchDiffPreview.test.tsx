/**
 * Tests for `SkillPatchDiffPreview`.
 *
 * Coverage per plan:
 * (a) diff preview renders all four panes
 * (b) Confirm button calls onConfirm
 * (c) Cancel button calls onCancel and does nothing else
 * (d) applySkillPatchWithUndo pushes a snapshot onto the undo stack on success
 * (e) undo restores both files (tested via popUndoEntry after a mock apply)
 */

import { cleanup, render, screen, fireEvent } from "@testing-library/react";
import { afterEach, describe, it, expect, vi, beforeEach } from "vitest";

// Mock Tauri
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async () => () => {}),
}));
vi.mock("@tauri-apps/api/webviewWindow", () => ({
  WebviewWindow: class {
    static async getByLabel() {
      return null;
    }
  },
}));
vi.mock("@tauri-apps/api/window", () => ({
  currentMonitor: async () => null,
}));

// Mock bindings — applySkillPatch resolves ok by default.
const mockApplySkillPatch = vi.fn(
  async (_arg: unknown): Promise<{ status: "ok"; data: null } | { status: "error"; error: unknown }> => ({
    status: "ok",
    data: null,
  }),
);
vi.mock("../../bindings", () => ({
  commands: {
    applySkillPatch: (arg: unknown) => mockApplySkillPatch(arg),
  },
}));

import {
  SkillPatchDiffPreview,
  type SkillPatchDiffInput,
} from "./SkillPatchDiffPreview";
import {
  applySkillPatchWithUndo,
  clearUndoStack,
  popUndoEntry,
  undoDepth,
} from "../../store/skills/SkillPatch";
import type { ApplySkillPatchRequest } from "../../bindings";

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

const diff: SkillPatchDiffInput = {
  markdownBefore: "Click Save.",
  markdownAfter: "Click Submit.",
  actionSketchBefore: '[\n  { "step_id": "s_001" }\n]',
  actionSketchAfter: '[\n  { "step_id": "s_001", "tool": "click" }\n]',
  variablesBefore: "[]",
  variablesAfter: '[{ "name": "target" }]',
  replayBefore: "{}",
  replayAfter: '{ "skill_id": "skl_x" }',
};

// (a) renders all four panes
describe("SkillPatchDiffPreview — rendering", () => {
  it("renders all four pane labels", () => {
    render(
      <SkillPatchDiffPreview
        diff={diff}
        onConfirm={vi.fn()}
        onCancel={vi.fn()}
      />,
    );
    expect(
      screen.getByText(/Markdown \(prose body\)/i),
    ).toBeDefined();
    expect(
      screen.getByText(/action_sketch \(JSON\)/i),
    ).toBeDefined();
    expect(screen.getByText(/variables/i)).toBeDefined();
    expect(
      screen.getByText(/replay\.json \(sidecar\)/i),
    ).toBeDefined();
  });
});

// (b) Confirm dispatches onConfirm
describe("SkillPatchDiffPreview — confirm", () => {
  it("Confirm button calls onConfirm", () => {
    const onConfirm = vi.fn();
    render(
      <SkillPatchDiffPreview
        diff={diff}
        onConfirm={onConfirm}
        onCancel={vi.fn()}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: /confirm/i }));
    expect(onConfirm).toHaveBeenCalledOnce();
  });
});

// (c) Cancel calls onCancel and not onConfirm
describe("SkillPatchDiffPreview — cancel", () => {
  it("Cancel button calls onCancel and not onConfirm", () => {
    const onConfirm = vi.fn();
    const onCancel = vi.fn();
    render(
      <SkillPatchDiffPreview
        diff={diff}
        onConfirm={onConfirm}
        onCancel={onCancel}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: /cancel/i }));
    expect(onCancel).toHaveBeenCalledOnce();
    expect(onConfirm).not.toHaveBeenCalled();
  });
});

// (d) successful apply pushes a snapshot onto the undo stack
describe("applySkillPatchWithUndo — undo stack integration", () => {
  const SKILL_ID = "skl_diff_test";

  beforeEach(() => {
    clearUndoStack(SKILL_ID);
    mockApplySkillPatch.mockResolvedValue({ status: "ok", data: null });
  });

  it("pushes a snapshot when apply succeeds", async () => {
    const request = {
      skill_id: SKILL_ID,
      version: 1,
      expected_mtime_ms: null,
      markdown_replacements: [],
      action_sketch_replacements: [],
      variables_additions: [],
      replay_sidecar_mutations: [],
      primitive: "free_form_prose" as const,
      project_path: null,
      project_name: "proj",
      project_id: "proj-id",
      store_traces: true,
    } satisfies ApplySkillPatchRequest;

    const snapshot = {
      skill_md_before: "before content",
      replay_json_before: null,
    };

    const result = await applySkillPatchWithUndo(request, snapshot);
    expect(result.ok).toBe(true);
    expect(undoDepth(SKILL_ID)).toBe(1);
  });

  // (e) undo restores both files — tested by verifying the snapshot content
  it("popped snapshot holds the pre-patch content", async () => {
    const request = {
      skill_id: SKILL_ID,
      version: 1,
      expected_mtime_ms: null,
      markdown_replacements: [],
      action_sketch_replacements: [],
      variables_additions: [],
      replay_sidecar_mutations: [],
      primitive: "free_form_prose" as const,
      project_path: null,
      project_name: "proj",
      project_id: "proj-id",
      store_traces: true,
    } satisfies ApplySkillPatchRequest;

    await applySkillPatchWithUndo(request, {
      skill_md_before: "## Old Section\n\nOld prose.",
      replay_json_before: '{"skill_id":"skl_diff_test"}',
    });

    const restored = popUndoEntry(SKILL_ID);
    expect(restored?.skill_md_before).toBe("## Old Section\n\nOld prose.");
    expect(restored?.replay_json_before).toBe('{"skill_id":"skl_diff_test"}');
  });

  it("does not push snapshot when apply fails", async () => {
    mockApplySkillPatch.mockResolvedValue({
      status: "error" as const,
      error: { kind: "Io", message: "disk full" },
    });

    const request = {
      skill_id: SKILL_ID,
      version: 1,
      expected_mtime_ms: null,
      markdown_replacements: [],
      action_sketch_replacements: [],
      variables_additions: [],
      replay_sidecar_mutations: [],
      primitive: "free_form_prose" as const,
      project_path: null,
      project_name: "proj",
      project_id: "proj-id",
      store_traces: true,
    } satisfies ApplySkillPatchRequest;

    const result = await applySkillPatchWithUndo(request, {
      skill_md_before: "before",
      replay_json_before: null,
    });
    expect(result.ok).toBe(false);
    expect(undoDepth(SKILL_ID)).toBe(0);
  });
});
