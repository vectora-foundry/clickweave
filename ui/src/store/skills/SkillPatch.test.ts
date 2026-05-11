/**
 * Tests for the per-skill in-memory undo stack (`SkillPatch.ts`).
 *
 * Coverage:
 * (a) pushUndoEntry respects the UNDO_STACK_CAP with FIFO eviction
 * (b) popUndoEntry returns the most-recent entry
 * (c) undoDepth tracks correctly after push/pop
 * (d) clearUndoStack empties the stack
 * (e) peekUndoEntry does not consume the entry
 */

import { describe, it, expect, beforeEach } from "vitest";
import {
  UNDO_STACK_CAP,
  clearUndoStack,
  peekUndoEntry,
  popUndoEntry,
  pushUndoEntry,
  undoDepth,
  type SkillUndoEntry,
} from "./SkillPatch";

const SKILL_ID = "skl_test_undo";

function entry(n: number): SkillUndoEntry {
  return { skill_md_before: `body-${n}`, replay_json_before: null };
}

beforeEach(() => {
  clearUndoStack(SKILL_ID);
});

describe("undo stack — push and pop", () => {
  it("pop returns the most-recently pushed entry", () => {
    pushUndoEntry(SKILL_ID, entry(1));
    pushUndoEntry(SKILL_ID, entry(2));
    const popped = popUndoEntry(SKILL_ID);
    expect(popped?.skill_md_before).toBe("body-2");
  });

  it("pop returns undefined when stack is empty", () => {
    expect(popUndoEntry(SKILL_ID)).toBeUndefined();
  });
});

describe("undo stack — depth tracking", () => {
  it("depth increases after push and decreases after pop", () => {
    expect(undoDepth(SKILL_ID)).toBe(0);
    pushUndoEntry(SKILL_ID, entry(1));
    expect(undoDepth(SKILL_ID)).toBe(1);
    pushUndoEntry(SKILL_ID, entry(2));
    expect(undoDepth(SKILL_ID)).toBe(2);
    popUndoEntry(SKILL_ID);
    expect(undoDepth(SKILL_ID)).toBe(1);
  });
});

describe("undo stack — cap eviction", () => {
  it("evicts oldest entry when cap is exceeded (FIFO)", () => {
    for (let i = 0; i < UNDO_STACK_CAP + 3; i++) {
      pushUndoEntry(SKILL_ID, entry(i));
    }
    expect(undoDepth(SKILL_ID)).toBe(UNDO_STACK_CAP);
    // Oldest entries (0, 1, 2) were evicted; stack[0] should now be entry(3).
    // We pop UNDO_STACK_CAP - 1 times to reach the bottom.
    for (let i = 0; i < UNDO_STACK_CAP - 1; i++) {
      popUndoEntry(SKILL_ID);
    }
    const oldest = popUndoEntry(SKILL_ID);
    expect(oldest?.skill_md_before).toBe(`body-${3}`);
  });
});

describe("undo stack — peek", () => {
  it("peek does not consume the entry", () => {
    pushUndoEntry(SKILL_ID, entry(42));
    const first = peekUndoEntry(SKILL_ID);
    const second = peekUndoEntry(SKILL_ID);
    expect(first?.skill_md_before).toBe("body-42");
    expect(second?.skill_md_before).toBe("body-42");
    expect(undoDepth(SKILL_ID)).toBe(1);
  });
});

describe("undo stack — clear", () => {
  it("clearUndoStack empties the stack", () => {
    pushUndoEntry(SKILL_ID, entry(1));
    pushUndoEntry(SKILL_ID, entry(2));
    clearUndoStack(SKILL_ID);
    expect(undoDepth(SKILL_ID)).toBe(0);
    expect(popUndoEntry(SKILL_ID)).toBeUndefined();
  });
});
