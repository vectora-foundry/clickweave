import type { Node, TargetCandidate, WalkthroughAction } from "../bindings";

/** Accessibility roles that represent specific, actionable UI elements (mirrors Rust ACTIONABLE_AX_ROLES). */
export const ACTIONABLE_AX_ROLES = new Set([
  "AXButton", "AXCheckBox", "AXComboBox", "AXDisclosureTriangle", "AXIncrementor",
  "AXLink", "AXMenuButton", "AXMenuItem", "AXPopUpButton", "AXRadioButton",
  "AXSegmentedControl", "AXSlider", "AXStaticText", "AXTab", "AXTabButton",
  "AXTextField", "AXTextArea", "AXToggle", "AXToolbarButton",
]);

/** Find the index of the preferred target candidate, mirroring backend `synthesize_draft` logic.
 *  Priority: actionable AX label > VlmLabel/OcrText > ImageCrop > Coordinates. */
export function preferredTargetIndex(candidates: TargetCandidate[]): number {
  // CDP-verified elements are most reliable
  const cdpIdx = candidates.findIndex((c) => c.type === "CdpElement");
  if (cdpIdx >= 0) return cdpIdx;
  const idx = candidates.findIndex((c) => {
    if (c.type === "AccessibilityLabel") return ACTIONABLE_AX_ROLES.has(c.role ?? "");
    return c.type === "VlmLabel" || c.type === "OcrText";
  });
  if (idx >= 0) return idx;
  // No text target — prefer ImageCrop over Coordinates (matching draft synthesis).
  const cropIdx = candidates.findIndex((c) => c.type === "ImageCrop");
  if (cropIdx >= 0) return cropIdx;
  const coordIdx = candidates.findIndex((c) => c.type === "Coordinates");
  return coordIdx >= 0 ? coordIdx : 0;
}

export function actionIcon(kind: WalkthroughAction["kind"]): { icon: string; color: string } {
  switch (kind.type) {
    case "LaunchApp": return { icon: "\u2B21", color: "text-green-400" };
    case "FocusWindow": return { icon: "\u25CE", color: "text-green-400" };
    case "Click": return { icon: "\u25C9", color: "text-[var(--accent-coral)]" };
    case "TypeText": return { icon: "\u2328", color: "text-blue-400" };
    case "PressKey": return { icon: "\u2325", color: "text-[var(--text-muted)]" };
    case "Scroll": return { icon: "\u2195", color: "text-[var(--text-muted)]" };
  }
}

export function actionLabel(action: WalkthroughAction): string {
  const k = action.kind;
  switch (k.type) {
    case "LaunchApp": return `Launch ${k.app_name}`;
    case "FocusWindow": return `Focus ${k.app_name}`;
    case "Click": {
      const idx = preferredTargetIndex(action.target_candidates);
      const best = action.target_candidates[idx];
      if (best && best.type !== "Coordinates" && best.type !== "ImageCrop") {
        const label = (best.type === "OcrText" || best.type === "CdpElement") ? best.text : best.label;
        return `Click '${label.length > 25 ? label.slice(0, 25) + "\u2026" : label}'`;
      }
      return `Click (${k.x}, ${k.y})`;
    }
    case "TypeText": {
      const t = k.text;
      return `Type '${t.length > 30 ? t.slice(0, 30) + "\u2026" : t}'`;
    }
    case "PressKey": {
      const mods = k.modifiers.length > 0 ? k.modifiers.join("+") + "+" : "";
      return `Press ${mods}${k.key}`;
    }
    case "Scroll": return "Scroll";
  }
}

export function targetCandidateLabel(candidate: TargetCandidate): string {
  switch (candidate.type) {
    case "AccessibilityLabel": return `"${candidate.label}"`;
    case "VlmLabel": return `"${candidate.label}"`;
    case "OcrText": return `"${candidate.text}"`;
    case "ImageCrop": return "Image crop";
    case "Coordinates": return `(${candidate.x}, ${candidate.y})`;
    case "CdpElement": return `"${candidate.text}"`;
  }
}

export function targetCandidateMethod(candidate: TargetCandidate): string {
  switch (candidate.type) {
    case "AccessibilityLabel": return "Accessibility";
    case "VlmLabel": return "Vision model";
    case "OcrText": return "OCR";
    case "ImageCrop": return "Image template";
    case "Coordinates": return "Screen coordinates";
    case "CdpElement": return "DevTools DOM";
  }
}

export function targetCandidateIcon(candidate: TargetCandidate): string {
  switch (candidate.type) {
    case "AccessibilityLabel": return "\u{1F3F7}";
    case "VlmLabel": return "\u{1F52D}";
    case "OcrText": return "\u{1F441}";
    case "ImageCrop": return "\u{1F5BC}";
    case "Coordinates": return "\u{1F4CD}";
    case "CdpElement": return "\u{1F310}";
  }
}

export function nodeTypeIcon(nodeType: Node["node_type"]): { icon: string; color: string } {
  switch (nodeType.type) {
    case "FocusWindow":
    case "ListWindows": return { icon: "\u25CE", color: "text-green-400" };
    case "Click": return { icon: "\u25C9", color: "text-[var(--accent-coral)]" };
    case "TypeText": return { icon: "\u2328", color: "text-blue-400" };
    case "PressKey": return { icon: "\u2325", color: "text-[var(--text-muted)]" };
    case "Scroll": return { icon: "\u2195", color: "text-[var(--text-muted)]" };
    case "AiStep": return { icon: "\u2605", color: "text-purple-400" };
    case "McpToolCall": return { icon: "\u2699", color: "text-blue-400" };
    case "TakeScreenshot":
    case "FindText":
    case "FindImage": return { icon: "\u25C7", color: "text-[var(--text-muted)]" };
    case "AppDebugKitOp": return { icon: "\u2699", color: "text-[var(--text-muted)]" };
    case "If":
    case "Switch":
    case "Loop":
    case "EndLoop": return { icon: "\u25C6", color: "text-yellow-400" };
  }
}

export function confidenceDot(confidence: WalkthroughAction["confidence"]): string {
  switch (confidence) {
    case "High": return "bg-green-400";
    case "Medium": return "bg-yellow-400";
    case "Low": return "bg-red-400";
  }
}

/** Compute crosshair position as percent of image dimensions, or null if not applicable. */
export function computeCrosshairPercent(
  action: WalkthroughAction,
  naturalWidth: number,
  naturalHeight: number,
): { xPercent: number; yPercent: number } | null {
  if (action.kind.type !== "Click" || !action.screenshot_meta) return null;
  const meta = action.screenshot_meta;
  const px = (action.kind.x - meta.origin_x) * meta.scale;
  const py = (action.kind.y - meta.origin_y) * meta.scale;
  if (naturalWidth <= 0 || naturalHeight <= 0) return null;
  return {
    xPercent: (px / naturalWidth) * 100,
    yPercent: (py / naturalHeight) * 100,
  };
}
