/**
 * Per-skill in-memory undo stack for SkillPatch operations.
 *
 * Each entry holds a paired snapshot of `{ skill_md_before, replay_json_before }`
 * captured immediately before a successful `apply_skill_patch` call. The stack
 * is capped at 16 entries (FIFO eviction) per skill so memory usage stays
 * bounded regardless of how many patches are applied in a session.
 *
 * The undo operation restores both files atomically by calling
 * `apply_skill_patch` with the pre-patch content reconstructed as a single
 * markdown replacement over the full body.
 */

import { commands } from "../../bindings";
import type { ApplySkillPatchRequest } from "../../bindings";

/** Maximum undo stack depth per skill. */
export const UNDO_STACK_CAP = 16;

/** A single undo snapshot captured before a patch apply. */
export interface SkillUndoEntry {
  /** The full SKILL.md text before the patch was applied. */
  skill_md_before: string;
  /** The full replay.json text (stringified) before the patch, or null if the
   *  sidecar was absent. */
  replay_json_before: string | null;
}

/** The per-skill undo stack map. Keyed by skill_id. */
const undoStacks = new Map<string, SkillUndoEntry[]>();

/** Push a snapshot onto the undo stack for `skill_id`. Evicts oldest when cap reached. */
export function pushUndoEntry(skill_id: string, entry: SkillUndoEntry): void {
  let stack = undoStacks.get(skill_id);
  if (!stack) {
    stack = [];
    undoStacks.set(skill_id, stack);
  }
  stack.push(entry);
  if (stack.length > UNDO_STACK_CAP) {
    stack.shift(); // FIFO eviction
  }
}

/** Peek at the top of the undo stack (most-recent snapshot) without consuming it. */
export function peekUndoEntry(skill_id: string): SkillUndoEntry | undefined {
  return undoStacks.get(skill_id)?.at(-1);
}

/** Pop and return the most-recent snapshot for `skill_id`, or undefined when the stack is empty. */
export function popUndoEntry(skill_id: string): SkillUndoEntry | undefined {
  return undoStacks.get(skill_id)?.pop();
}

/** Current undo stack depth for a skill. */
export function undoDepth(skill_id: string): number {
  return undoStacks.get(skill_id)?.length ?? 0;
}

/** Clear the undo stack for a skill (e.g. after a save or discard). */
export function clearUndoStack(skill_id: string): void {
  undoStacks.delete(skill_id);
}

// ── Patch application helpers ─────────────────────────────────────────────

/** Context required to call `apply_skill_patch` via Tauri IPC. */
export interface SkillPatchContext {
  projectPath: string | null;
  projectName: string;
  projectId: string;
  storeTraces: boolean;
}

/**
 * Apply a patch and push an undo snapshot on success.
 *
 * @param snapshot - The pre-patch snapshot to push on the undo stack if the
 *   apply succeeds. Pass `null` to skip undo tracking (e.g. for redo).
 */
export async function applySkillPatchWithUndo(
  request: ApplySkillPatchRequest,
  snapshot: SkillUndoEntry | null,
): Promise<{ ok: true } | { ok: false; error: string }> {
  const result = await commands.applySkillPatch(request);
  if (result.status === "error") {
    const msg =
      typeof result.error === "object" &&
      result.error !== null &&
      "message" in result.error
        ? String((result.error as { message: unknown }).message)
        : String(result.error);
    return { ok: false, error: msg };
  }
  if (snapshot) {
    pushUndoEntry(request.skill_id, snapshot);
  }
  return { ok: true };
}
