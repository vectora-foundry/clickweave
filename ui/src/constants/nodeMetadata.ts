/** Shared node metadata for colors and short icon labels used in the graph canvas and walkthrough panel. */
export const nodeMetadata: Record<string, { color: string; icon: string }> = {
  AiStep:         { color: "#4c9ee8", icon: "AI" },
  TakeScreenshot: { color: "#a855f7", icon: "SS" },
  FindText:       { color: "#a855f7", icon: "FT" },
  FindImage:      { color: "#a855f7", icon: "FI" },
  Click:          { color: "#f59e0b", icon: "CK" },
  Hover:          { color: "#f59e0b", icon: "HV" },
  TypeText:       { color: "#f59e0b", icon: "TT" },
  Scroll:         { color: "#f59e0b", icon: "SC" },
  ListWindows:    { color: "#50c878", icon: "LW" },
  FocusWindow:    { color: "#50c878", icon: "FW" },
  PressKey:       { color: "#f59e0b", icon: "PK" },
  McpToolCall:    { color: "#666",    icon: "MC" },
  AppDebugKitOp:  { color: "#ef4444", icon: "DK" },
  If:             { color: "#10b981", icon: "IF" },
  Switch:         { color: "#10b981", icon: "SW" },
  Loop:           { color: "#10b981", icon: "LP" },
  EndLoop:        { color: "#10b981", icon: "EL" },
};

export const defaultNodeMetadata = { color: "#666", icon: "??" };
