import type { AppKind } from "../bindings";

export const APP_KIND_LABELS: Record<AppKind, string> = {
  Native: "Native (Accessibility)",
  ChromeBrowser: "Chrome DevTools",
  ElectronApp: "Electron (DevTools)",
};

/** Whether an AppKind uses Chrome DevTools Protocol for automation. */
export function usesCdp(kind: AppKind): boolean {
  return kind === "ChromeBrowser" || kind === "ElectronApp";
}
