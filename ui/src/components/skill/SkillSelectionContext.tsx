/**
 * `SkillSelectionContext` — multi-select state for `SkillView` section cards.
 *
 * Implements D25 selection model:
 * - `selectSingle(id)` — single-select toggle (deselects everything else, or
 *   deselects the item if it was the only selection)
 * - `extendRange(id, allIds)` — shift-click contiguous range from the last
 *   anchor to `id`
 * - `toggleMulti(id)` — ⌘/Ctrl-click toggle: adds if absent, removes if present
 * - `clear()` — deselect all
 *
 * The context is keyed on `skillId` so state resets when the selected skill
 * changes.
 */

import {
  createContext,
  useCallback,
  useContext,
  useRef,
  useState,
} from "react";
import { useStore } from "../../store/useAppStore";

export interface SkillSelectionState {
  selectedSectionIds: string[];
  selectedSkillId: string | null;
  /** "Editing" during normal operation; "Inspecting" when the skill is frozen (run in progress). */
  mode: "Editing" | "Inspecting";
  selectSingle: (sectionId: string) => void;
  extendRange: (sectionId: string, allIds: string[]) => void;
  toggleMulti: (sectionId: string) => void;
  clear: () => void;
}

const SkillSelectionContext = createContext<SkillSelectionState>({
  selectedSectionIds: [],
  selectedSkillId: null,
  mode: "Editing",
  selectSingle: () => {},
  extendRange: () => {},
  toggleMulti: () => {},
  clear: () => {},
});

interface SkillSelectionProviderProps {
  skillId: string;
  children: React.ReactNode;
}

export function SkillSelectionProvider({
  skillId,
  children,
}: SkillSelectionProviderProps) {
  const [selectedSectionIds, setSelectedSectionIds] = useState<string[]>([]);
  // Anchor is the last "primary" click target — used for shift-click range.
  const anchorRef = useRef<string | null>(null);
  const skillFrozen = useStore((s) => s.skillFrozen);
  const mode: "Editing" | "Inspecting" = skillFrozen ? "Inspecting" : "Editing";

  const selectSingle = useCallback((sectionId: string) => {
    // Selection mutations are disabled while the skill is frozen (run in progress).
    if (useStore.getState().skillFrozen) return;
    setSelectedSectionIds((prev) => {
      if (prev.length === 1 && prev[0] === sectionId) {
        // Toggle: clicking the only selected item deselects it.
        anchorRef.current = null;
        return [];
      }
      anchorRef.current = sectionId;
      return [sectionId];
    });
  }, []);

  const extendRange = useCallback((sectionId: string, allIds: string[]) => {
    if (useStore.getState().skillFrozen) return;
    const anchor = anchorRef.current;
    if (!anchor) {
      // No anchor — treat as single select.
      anchorRef.current = sectionId;
      setSelectedSectionIds([sectionId]);
      return;
    }
    const anchorIdx = allIds.indexOf(anchor);
    const targetIdx = allIds.indexOf(sectionId);
    if (anchorIdx === -1 || targetIdx === -1) {
      setSelectedSectionIds([sectionId]);
      return;
    }
    const lo = Math.min(anchorIdx, targetIdx);
    const hi = Math.max(anchorIdx, targetIdx);
    setSelectedSectionIds(allIds.slice(lo, hi + 1));
    // Anchor stays at the original point so successive shift-clicks extend
    // from the same origin.
  }, []);

  const toggleMulti = useCallback((sectionId: string) => {
    if (useStore.getState().skillFrozen) return;
    setSelectedSectionIds((prev) => {
      if (prev.includes(sectionId)) {
        const next = prev.filter((id) => id !== sectionId);
        // Update anchor to the last remaining item.
        anchorRef.current = next[next.length - 1] ?? null;
        return next;
      }
      anchorRef.current = sectionId;
      return [...prev, sectionId];
    });
  }, []);

  const clear = useCallback(() => {
    anchorRef.current = null;
    setSelectedSectionIds([]);
  }, []);

  return (
    <SkillSelectionContext.Provider
      value={{
        selectedSectionIds,
        selectedSkillId: skillId,
        mode,
        selectSingle,
        extendRange,
        toggleMulti,
        clear,
      }}
    >
      {children}
    </SkillSelectionContext.Provider>
  );
}

export function useSkillSelection(): SkillSelectionState {
  return useContext(SkillSelectionContext);
}
