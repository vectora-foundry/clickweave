import { useEffect } from "react";
import { useStore } from "../store/useAppStore";

export function isTextInput(el: Element | null): boolean {
  if (!el) return false;
  const tag = el.tagName;
  if (tag === "INPUT" || tag === "TEXTAREA") return true;
  if (el instanceof HTMLElement && el.isContentEditable) return true;
  return false;
}

export function useUndoRedoKeyboard(undo: () => void, redo: () => void) {
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      const { executorState } = useStore.getState();
      if (executorState === "running") return;
      if (isTextInput(document.activeElement)) return;
      const isMod = e.metaKey || e.ctrlKey;
      if (!isMod || e.key.toLowerCase() !== "z") return;

      e.preventDefault();
      if (e.shiftKey) {
        redo();
      } else {
        undo();
      }
    };

    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [undo, redo]);
}
