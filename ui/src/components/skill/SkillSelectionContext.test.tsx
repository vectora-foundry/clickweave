/**
 * Tests for `SkillSelectionContext`.
 *
 * Coverage per plan:
 * - `selectSingle` — toggle on/off, replaces multi-selection
 * - `extendRange` — shift-click contiguous range
 * - `toggleMulti` — ⌘/Ctrl-click toggle
 * - `clear` — deselects all
 */

import { act, renderHook } from "@testing-library/react";
import { describe, it, expect } from "vitest";
import { SkillSelectionProvider, useSkillSelection } from "./SkillSelectionContext";

function wrapper({ children }: { children: React.ReactNode }) {
  return <SkillSelectionProvider skillId="skl_test">{children}</SkillSelectionProvider>;
}

describe("SkillSelectionContext.selectSingle", () => {
  it("selects a single section id", () => {
    const { result } = renderHook(() => useSkillSelection(), { wrapper });
    act(() => result.current.selectSingle("s1"));
    expect(result.current.selectedSectionIds).toEqual(["s1"]);
  });

  it("deselects when the only selected item is clicked again", () => {
    const { result } = renderHook(() => useSkillSelection(), { wrapper });
    act(() => result.current.selectSingle("s1"));
    act(() => result.current.selectSingle("s1"));
    expect(result.current.selectedSectionIds).toEqual([]);
  });

  it("replaces the existing selection with the new id", () => {
    const { result } = renderHook(() => useSkillSelection(), { wrapper });
    act(() => result.current.selectSingle("s1"));
    act(() => result.current.selectSingle("s2"));
    expect(result.current.selectedSectionIds).toEqual(["s2"]);
  });
});

describe("SkillSelectionContext.extendRange", () => {
  const allIds = ["s1", "s2", "s3", "s4", "s5"];

  it("selects contiguous range from anchor to target", () => {
    const { result } = renderHook(() => useSkillSelection(), { wrapper });
    act(() => result.current.selectSingle("s2")); // anchor
    act(() => result.current.extendRange("s4", allIds));
    expect(result.current.selectedSectionIds).toEqual(["s2", "s3", "s4"]);
  });

  it("extends range backwards from anchor", () => {
    const { result } = renderHook(() => useSkillSelection(), { wrapper });
    act(() => result.current.selectSingle("s4")); // anchor
    act(() => result.current.extendRange("s2", allIds));
    expect(result.current.selectedSectionIds).toEqual(["s2", "s3", "s4"]);
  });

  it("falls back to single-select when there is no anchor", () => {
    const { result } = renderHook(() => useSkillSelection(), { wrapper });
    act(() => result.current.extendRange("s3", allIds));
    expect(result.current.selectedSectionIds).toEqual(["s3"]);
  });
});

describe("SkillSelectionContext.toggleMulti", () => {
  it("adds an id not yet in the selection", () => {
    const { result } = renderHook(() => useSkillSelection(), { wrapper });
    act(() => result.current.selectSingle("s1"));
    act(() => result.current.toggleMulti("s3"));
    expect(result.current.selectedSectionIds).toContain("s1");
    expect(result.current.selectedSectionIds).toContain("s3");
  });

  it("removes an id already in the selection", () => {
    const { result } = renderHook(() => useSkillSelection(), { wrapper });
    act(() => result.current.selectSingle("s1"));
    act(() => result.current.toggleMulti("s3"));
    act(() => result.current.toggleMulti("s3"));
    expect(result.current.selectedSectionIds).not.toContain("s3");
    expect(result.current.selectedSectionIds).toContain("s1");
  });
});

describe("SkillSelectionContext.clear", () => {
  it("deselects all ids", () => {
    const { result } = renderHook(() => useSkillSelection(), { wrapper });
    act(() => result.current.selectSingle("s1"));
    act(() => result.current.toggleMulti("s2"));
    act(() => result.current.clear());
    expect(result.current.selectedSectionIds).toEqual([]);
  });
});
